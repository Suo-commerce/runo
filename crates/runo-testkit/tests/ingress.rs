//! Ingress (technical spec §4, steps 1–14) driven against `MemStore`, with every decision
//! applied through the same routes the node will use.

#![allow(
    clippy::many_single_char_names,
    reason = "store, chorus, lane, task and verse read better short in tests"
)]

use proptest::prelude::*;
use runo_core::ingress::{Decision, RecordClass, RejectReason, admit};
use runo_core::{Epoch, Payload, SingerId, TaskId, Verse};
use runo_store::mem::MemStore;
use runo_store::{Cursor, ResourceFenceStore, VerseReader};
use runo_testkit::fixture::{ALLOCATOR, Chorus, lane, route};

fn t(name: &str) -> TaskId {
    TaskId(name.into())
}

/// Admit and apply; returns the decision.
async fn sing(store: &MemStore, chorus: &Chorus, v: &Verse) -> Decision {
    let d = admit(v, chorus, store);
    route(store, v, &d)
        .await
        .expect("the route accepts what ingress decided");
    d
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
}

#[tokio::test]
async fn a_chain_appends_and_a_reread_is_stored_with_its_class() {
    let (store, mut c) = (MemStore::new(), Chorus::new(1));
    let a = c.claim(&lane(0), &t("A"), Epoch(0), None);
    let b = c.work(&lane(0), &t("A"), Epoch(0), None);
    assert!(matches!(
        sing(&store, &c, &a).await,
        Decision::Append { fork_of: None, .. }
    ));
    assert!(matches!(
        sing(&store, &c, &b).await,
        Decision::Append { fork_of: None, .. }
    ));
    assert_eq!(
        admit(&a, &c, &store),
        Decision::Stored {
            class: RecordClass::Current
        }
    );
}

#[tokio::test]
async fn integrity_failures_are_rejected_in_order() {
    let (store, mut c) = (MemStore::new(), Chorus::new(2));
    let good = c.claim(&lane(0), &t("I"), Epoch(0), None);

    let mut stranger = good.clone();
    stranger.header.singer = SingerId("nobody".into());
    assert_eq!(
        admit(&stranger, &c, &store),
        Decision::Reject(RejectReason::UnknownSinger)
    );

    let mut tampered = good.clone();
    tampered.header.note = Some("edited".into());
    assert_eq!(
        admit(&tampered, &c, &store),
        Decision::Reject(RejectReason::BadSum)
    );

    // lane-1 signs a verse that claims to be lane-0's.
    let mut forged = good.clone();
    let sum = forged.seal.sum;
    forged.seal.sig = ed25519_dalek::Signer::sign(c.key(&lane(1)), &(sum.0).0).to_bytes();
    assert_eq!(
        admit(&forged, &c, &store),
        Decision::Reject(RejectReason::BadSig)
    );

    let mut future = good.clone();
    future.header.v = 4;
    assert_eq!(
        admit(&future, &c, &store),
        Decision::Reject(RejectReason::Unknown)
    );
}

#[tokio::test]
async fn record_kinds_are_privileged() {
    let (store, mut c) = (MemStore::new(), Chorus::new(1));
    let payload = Payload::Grant {
        holder: lane(0),
        task: t("P"),
        epoch: Epoch(1),
        resources: vec![],
    };
    let self_grant = c.sing(&lane(0), &t("P"), Epoch(0), None, None, payload);
    assert_eq!(
        admit(&self_grant, &c, &store),
        Decision::Reject(RejectReason::Privilege)
    );
}

#[tokio::test]
async fn a_stale_roster_may_archive_but_not_act() {
    let (store, mut c) = (MemStore::new(), Chorus::new(1));
    let old = c.claim(&lane(0), &t("R"), Epoch(0), None);
    c.next_roster();
    assert_eq!(
        admit(&old, &c, &store),
        Decision::Reject(RejectReason::StaleRoster)
    );
}

#[tokio::test]
async fn missing_predecessors_hold_the_verse_pending_and_a_gap_is_a_bad_chain() {
    let (store, mut c) = (MemStore::new(), Chorus::new(1));
    let a = c.claim(&lane(0), &t("C"), Epoch(0), None);
    let b = c.work(&lane(0), &t("C"), Epoch(0), None);
    assert_eq!(
        admit(&b, &c, &store),
        Decision::Pending {
            waiting_for: vec![a.seal.sum]
        }
    );
    sing(&store, &c, &a).await;
    sing(&store, &c, &b).await;

    // A verse at seq 3 whose prev is seq 0: the predecessor exists but is not adjacent.
    let mut skip = c.work(&lane(0), &t("C"), Epoch(0), None);
    skip.header.seq = 3;
    skip.header.prev = Some(a.seal.sum);
    let skip = runo_core::sign::seal(c.key(&lane(0)), skip.header, skip.payload).unwrap();
    assert_eq!(
        admit(&skip, &c, &store),
        Decision::Reject(RejectReason::BadChain)
    );
}

#[tokio::test]
async fn activation_and_grant_rules() {
    let (store, mut c) = (MemStore::new(), Chorus::new(2));
    let (l, task) = (lane(0), t("G"));
    {
        let v = c.claim(&l, &task, Epoch(0), None);
        sing(&store, &c, &v).await
    };

    let g = c.grant(&l, &task, Epoch(1));
    let act = c.activate(&g);
    let new = c.claim(&l, &task, Epoch(1), Some(g.seal.sum));

    // The same chain position at epoch 1, with no grant.
    let mut h = new.header.clone();
    h.grant = None;
    let ungranted = runo_core::sign::seal(c.key(&l), h, new.payload.clone()).unwrap();
    assert_eq!(
        admit(&ungranted, &c, &store),
        Decision::Reject(RejectReason::BadGrant)
    );

    // An activation that arrives before its grant waits for it.
    assert!(matches!(admit(&act, &c, &store), Decision::Pending { .. }));
    {
        let v = g.clone();
        sing(&store, &c, &v).await
    };

    // The same chain position, with acks that do not cover the grant's resources.
    let short = runo_core::sign::seal(
        c.key(&SingerId(ALLOCATOR.into())),
        act.header.clone(),
        Payload::Activation {
            grant: g.seal.sum,
            acks: vec![],
        },
    )
    .unwrap();
    assert_eq!(
        admit(&short, &c, &store),
        Decision::Reject(RejectReason::BadActivation)
    );

    // A lane's verse naming the grant waits for its activation, then lands.
    store.install_fence(&g).await.unwrap();
    assert_eq!(
        admit(&new, &c, &store),
        Decision::Pending {
            waiting_for: vec![g.seal.sum]
        }
    );
    sing(&store, &c, &act).await;
    assert!(matches!(
        sing(&store, &c, &new).await,
        Decision::Append { .. }
    ));

    // Someone else's grant.
    let other = c.claim(&lane(1), &task, Epoch(1), Some(g.seal.sum));
    assert_eq!(
        admit(&other, &c, &store),
        Decision::Reject(RejectReason::BadGrant)
    );
}

#[tokio::test]
async fn relaunch_files_the_old_incarnation_as_history() {
    let (store, mut c) = (MemStore::new(), Chorus::new(1));
    let (l, task) = (lane(0), t("H"));
    {
        let v = c.claim(&l, &task, Epoch(0), None);
        sing(&store, &c, &v).await
    };
    let late = c.work(&l, &task, Epoch(0), None);

    let g = c.grant(&l, &task, Epoch(1));
    sing(&store, &c, &g).await;
    store.install_fence(&g).await.unwrap();
    {
        let v = c.activate(&g);
        sing(&store, &c, &v).await
    };
    let new = c.claim(&l, &task, Epoch(1), Some(g.seal.sum));
    assert!(matches!(
        sing(&store, &c, &new).await,
        Decision::Append { .. }
    ));

    assert_eq!(sing(&store, &c, &late).await, Decision::History);
    assert_eq!(
        admit(&late, &c, &store),
        Decision::Stored {
            class: RecordClass::History
        }
    );
}

#[tokio::test]
async fn closure_makes_control_records_and_history() {
    let (store, mut c) = (MemStore::new(), Chorus::new(1));
    let (l, task) = (lane(0), t("X"));
    {
        let v = c.claim(&l, &task, Epoch(0), None);
        sing(&store, &c, &v).await
    };
    let late = c.work(&l, &task, Epoch(0), None);
    let snap = runo_store::VerseWriter::put_blob(&store, b"snap")
        .await
        .unwrap();
    let close = c.close(&task, snap);
    store.install_fence(&close).await.unwrap();
    assert_eq!(sing(&store, &c, &close).await, Decision::Control);
    assert_eq!(sing(&store, &c, &late).await, Decision::History);
}

#[tokio::test]
async fn a_second_variant_at_a_seq_is_a_fork_and_both_are_kept() {
    let (store, mut c) = (MemStore::new(), Chorus::new(1));
    let (l, task) = (lane(0), t("F"));
    let a = c.claim(&l, &task, Epoch(0), None);
    sing(&store, &c, &a).await;
    let one = c.work(&l, &task, Epoch(0), None);
    let mut other = one.clone();
    other.header.note = Some("the other variant".into());
    let other = runo_core::sign::seal(c.key(&l), other.header, other.payload).unwrap();
    sing(&store, &c, &one).await;
    let d = sing(&store, &c, &other).await;
    assert!(matches!(d, Decision::Append { fork_of: Some(f), .. } if f == one.seal.sum));
    assert_eq!(
        store.read_since(Cursor::default()).await.unwrap().0.len(),
        3
    );
}

// The property the spec names: a shuffled valid chain, fed through a pending buffer, appends
// every verse exactly once and in seq order per chain.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn shuffled_chains_append_once_in_seq_order(len in 1usize..12, lanes in 1u8..4, seed in any::<u64>()) {
        let mut c = Chorus::new(lanes);
        let task = t("S");
        let mut verses = Vec::new();
        for i in 0..lanes {
            verses.push(c.claim(&lane(i), &task, Epoch(0), None));
            for _ in 1..len {
                verses.push(c.work(&lane(i), &task, Epoch(0), None));
            }
        }
        shuffle(&mut verses, seed);
        let store = MemStore::new();
        rt().block_on(async {
            let mut pending = verses;
            loop {
                let before = pending.len();
                let mut still = Vec::new();
                for v in pending {
                    match admit(&v, &c, &store) {
                        Decision::Pending { .. } => still.push(v),
                        d => {
                            let appended = matches!(d, Decision::Append { fork_of: None, .. });
                            prop_assert!(appended, "unexpected decision {:?}", d);
                            route(&store, &v, &d).await.unwrap();
                        }
                    }
                }
                pending = still;
                if pending.is_empty() || pending.len() == before {
                    break;
                }
            }
            prop_assert!(pending.is_empty());
            Ok(())
        })?;
        let (all, _) = rt().block_on(store.read_since(Cursor::default())).unwrap();
        prop_assert_eq!(all.len(), usize::from(lanes) * len);
        for i in 0..lanes {
            let seqs: Vec<u64> = all
                .iter()
                .filter(|r| r.verse.header.singer == lane(i))
                .map(|r| r.verse.header.seq)
                .collect();
            prop_assert_eq!(seqs, (0..len as u64).collect::<Vec<_>>());
        }
    }
}

fn shuffle(v: &mut [Verse], mut seed: u64) {
    for i in (1..v.len()).rev() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let j = usize::try_from(seed % (i as u64 + 1)).unwrap();
        v.swap(i, j);
    }
}
