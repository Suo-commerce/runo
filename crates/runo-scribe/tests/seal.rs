//! Sealing and verifying tomes over a real closure (technical spec §9, §12 `late-verse`).

#![allow(
    clippy::many_single_char_names,
    reason = "store, chorus, lane, task and allocator read better short in tests"
)]

use std::sync::Arc;

use runo_core::codec::decode;
use runo_core::fence::Durability;
use runo_core::ingress::{Decision, admit};
use runo_core::{
    Epoch, NetTime, Payload, ResourceId, SingerId, TaskId, Timestamp, TomeManifest, Verse,
};
use runo_fence::journal::MemJournal;
use runo_fence::{Allocator, Outcome};
use runo_scribe::{ScribeError, Tome, late, seal, verify};
use runo_store::mem::MemStore;
use runo_store::{ScribeRead, SongStore, VerseWriter};
use runo_testkit::fixture::{Chorus, lane};

fn store() -> Arc<MemStore> {
    Arc::new(MemStore::simulated(
        ResourceId("song".into()),
        Durability::Host,
    ))
}

fn allocator(c: &Chorus, s: Arc<dyn SongStore>) -> Allocator<MemJournal> {
    let key = c.key(&SingerId("allocator".into())).clone();
    let current = runo_core::ingress::RosterHistory::current(c);
    let roster = runo_core::ingress::RosterHistory::at(c, current)
        .unwrap()
        .clone();
    Allocator::new(
        key,
        roster,
        s,
        MemJournal::new(),
        Box::new(|| Timestamp("2026-11-03T12:30:00Z".into())),
    )
    .unwrap()
}

async fn put(s: &MemStore, c: &Chorus, v: &Verse) -> Decision {
    let d = admit(v, c, s);
    match &d {
        Decision::Append { .. } if v.header.grant.is_some() => {
            s.append_fenced(v, Durability::Host).await.unwrap();
        }
        Decision::Append { .. } => {
            s.append(v, Durability::Host).await.unwrap();
        }
        Decision::History => {
            s.archive(v, Durability::Host).await.unwrap();
        }
        _ => {}
    }
    d
}

/// Two lanes work a task, one forks, one is relaunched; the allocator closes it.
async fn closed_task(
    s: &Arc<MemStore>,
    c: &mut Chorus,
    t: &TaskId,
) -> (Verse, Allocator<MemJournal>) {
    for i in 0..2 {
        let v = c.claim(&lane(i), t, Epoch(0), None);
        put(s, c, &v).await;
        let v = c.work(&lane(i), t, Epoch(0), None);
        put(s, c, &v).await;
    }
    // lane-0 equivocates at seq 1.
    let one = s
        .task_records(t)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.verse)
        .find(|v| v.header.singer == lane(0) && v.header.seq == 1)
        .unwrap();
    let mut other = one.clone();
    other.header.note = Some("the other variant".into());
    let other = runo_core::sign::seal(c.key(&lane(0)), other.header, other.payload).unwrap();
    put(s, c, &other).await;
    let a = allocator(c, s.clone());
    let Outcome::Activated { grant, .. } = a.relaunch(&lane(1), t).await.unwrap() else {
        panic!()
    };
    let v = c.claim(&lane(1), t, Epoch(1), Some(grant));
    put(s, c, &v).await;
    let close = a.close(t).await.unwrap();
    (close, a)
}

#[tokio::test]
async fn a_sealed_tome_verifies_and_a_late_verse_does_not_change_it() {
    let (s, mut c) = (store(), Chorus::new(2));
    let t = TaskId("HOD-50".into());
    let (close, a) = closed_task(&s, &mut c, &t).await;

    let sealed = seal(&*s, &close).await.unwrap();
    s.put_blob(&sealed.manifest).await.unwrap();
    let manifest: TomeManifest = decode(&sealed.manifest).unwrap();
    assert_eq!(manifest.close, close.seal.sum);
    assert!(
        !manifest.snapshot.forks.is_empty(),
        "the fork is in the snapshot"
    );
    assert_eq!(
        sealed.included.len() as u64,
        manifest.summary.verse_count,
        "every chain walked back to seq 0, both fork branches included"
    );
    assert!(
        matches!(manifest.summary.timing.net, NetTime::Valid(_)),
        "{:?}",
        manifest.summary.timing
    );
    verify(&*s, &sealed.tome).await.unwrap();

    // The allocator carries the tome into the song.
    let tome = a
        .tome(&t, close.seal.sum, sealed.tome.manifest, sealed.tome.root)
        .await
        .unwrap();
    assert!(matches!(tome.payload, Payload::Tome { .. }));

    // late-verse: a signed verse arrives after closure; archived unchanged, an addendum
    // recorded, the tome unchanged and still verifying.
    let late_verse = c.work(&lane(0), &t, Epoch(0), None);
    assert_eq!(put(&s, &c, &late_verse).await, Decision::History);
    let records = s.task_records(&t).await.unwrap();
    let after = late(&sealed.included, &records);
    assert!(after.contains(&late_verse.seal.sum), "{after:?}");
    a.addendum(&t, tome.seal.sum, late_verse.seal.sum)
        .await
        .unwrap();
    verify(&*s, &sealed.tome).await.unwrap();
    assert_eq!(
        seal(&*s, &close).await.unwrap(),
        sealed,
        "the tome is unchanged"
    );
}

/// A second, independent reader over the same contents that hands records back in another
/// order.
struct Reversed<'a>(&'a MemStore);

#[async_trait::async_trait]
impl ScribeRead for Reversed<'_> {
    async fn snapshot(
        &self,
        close: &runo_core::VerseId,
    ) -> Result<Option<Vec<u8>>, runo_store::StoreError> {
        self.0.snapshot(close).await
    }
    async fn blob(&self, h: &runo_core::Hash) -> Result<Option<Vec<u8>>, runo_store::StoreError> {
        self.0.blob(h).await
    }
    async fn task_records(
        &self,
        task: &TaskId,
    ) -> Result<Vec<runo_store::Record>, runo_store::StoreError> {
        let mut r = self.0.task_records(task).await?;
        r.reverse();
        Ok(r)
    }
}

#[tokio::test]
async fn sealing_is_byte_identical_whatever_order_the_reader_gives() {
    let (s, mut c) = (store(), Chorus::new(2));
    let t = TaskId("HOD-51".into());
    let (close, _) = closed_task(&s, &mut c, &t).await;
    let sealed = seal(&*s, &close).await.unwrap();
    assert_eq!(seal(&Reversed(&s), &close).await.unwrap(), sealed);
}

#[tokio::test]
async fn a_tampered_tome_names_what_differs() {
    let (s, mut c) = (store(), Chorus::new(2));
    let t = TaskId("HOD-52".into());
    let (close, _) = closed_task(&s, &mut c, &t).await;
    let sealed = seal(&*s, &close).await.unwrap();
    s.put_blob(&sealed.manifest).await.unwrap();
    let forged = Tome {
        root: runo_core::Hash([7; 32]),
        ..sealed.tome.clone()
    };
    let err = verify(&*s, &forged).await.unwrap_err();
    assert!(
        matches!(&err, ScribeError::Mismatch(why) if why.starts_with("the root")),
        "{err}"
    );
}
