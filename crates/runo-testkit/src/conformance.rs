//! Conformance suites: the same tests against every implementation of a trait. A new
//! implementation is not done until its suite passes (CLAUDE.md, design rules).

/// Run the `SongStore` conformance suite against a store constructor and the highest
/// durability the backend provides. Each case is its own test, on its own fresh store.
#[macro_export]
macro_rules! store_conformance {
    ($name:ident, $make:expr, $level:expr) => {
        mod $name {
            #[allow(unused_imports)]
            use super::*;
            $crate::store_conformance!(@case reads_return_records_with_their_class, $make, $level);
            $crate::store_conformance!(@case reappend_keeps_the_first_class, $make, $level);
            $crate::store_conformance!(@case fences_route_current_and_history, $make, $level);
            $crate::store_conformance!(@case a_fence_never_lowers, $make, $level);
            $crate::store_conformance!(@case closure_routes_control, $make, $level);
            $crate::store_conformance!(@case head_is_scoped_per_task, $make, $level);
            $crate::store_conformance!(@case blobs_refrains_and_scribe_reads, $make, $level);
            $crate::store_conformance!(@case durability_is_honoured_or_refused, $make, $level);
        }
    };
    (@case $case:ident, $make:expr, $level:expr) => {
        #[tokio::test]
        async fn $case() {
            let store = $make;
            $crate::conformance::store::$case(&store, $level).await;
        }
    };
}

/// The bodies of the `SongStore` conformance cases (technical spec §7). Each takes a fresh store
/// and the highest durability the backend provides.
pub mod store {
    #![allow(
        clippy::missing_panics_doc,
        clippy::many_single_char_names,
        reason = "conformance cases are assertions: panicking is how they fail"
    )]

    use runo_core::fence::{Durability, Fence};
    use runo_core::hash::digest;
    use runo_core::ingress::{RecordClass, TaskState};
    use runo_core::{Epoch, ResourceId, SingerId, TaskId};
    use runo_store::{Cursor, FenceError, SongStore, StoreError};

    use crate::fixture::{ALLOCATOR, Chorus, lane};

    fn task(name: &str) -> TaskId {
        TaskId(name.into())
    }

    /// `read_since` and `get` return every record with its class and a cursor that reads on.
    pub async fn reads_return_records_with_their_class(s: &dyn SongStore, level: Durability) {
        let mut c = Chorus::new(1);
        let t = task("T-read");
        let a = c.claim(&lane(0), &t, Epoch(0), None);
        let b = c.work(&lane(0), &t, Epoch(0), None);
        s.append(&a, level).await.unwrap();
        let r = s.append(&b, level).await.unwrap();
        assert_eq!(r.id, b.seal.sum);
        assert!(r.level >= level);
        let (all, next) = s.read_since(Cursor::default()).await.unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().all(|r| r.class == RecordClass::Current));
        assert_eq!(all[0].verse, a);
        let (after_first, _) = s.read_since(all[0].cursor).await.unwrap();
        assert_eq!(after_first.len(), 1);
        assert_eq!(after_first[0].verse, b);
        assert!(s.read_since(next).await.unwrap().0.is_empty());
        let got = s.get(&b.seal.sum).await.unwrap().unwrap();
        assert_eq!((got.verse, got.class), (b, RecordClass::Current));
    }

    /// A byte-identical re-admission on any route is a no-op that keeps the first class.
    pub async fn reappend_keeps_the_first_class(s: &dyn SongStore, level: Durability) {
        let mut c = Chorus::new(1);
        let t = task("T-dedup");
        let a = c.claim(&lane(0), &t, Epoch(0), None);
        s.append(&a, level).await.unwrap();
        s.append(&a, level).await.unwrap();
        assert_eq!(s.read_since(Cursor::default()).await.unwrap().0.len(), 1);
    }

    /// After a fence: `append` refuses the chain, `append_fenced` takes the activated epoch,
    /// stale work is `Stale` and archives as `History`, and an archived verse re-appended stays
    /// `History`; `archive` refuses current work.
    pub async fn fences_route_current_and_history(s: &dyn SongStore, level: Durability) {
        let mut c = Chorus::new(1);
        let (l, t) = (lane(0), task("T-fence"));
        let claim0 = c.claim(&l, &t, Epoch(0), None);
        s.append(&claim0, level).await.unwrap();
        let unfenced_err = s.archive(&c.work(&l, &t, Epoch(0), None), level).await;
        assert!(
            matches!(unfenced_err, Err(StoreError::Route(_))),
            "archive refuses current work"
        );
        let old = c.work(&l, &t, Epoch(0), None);

        let g = c.grant(&l, &t, Epoch(1));
        s.append(&g, level).await.unwrap();
        let ack = s.install_fence(&g).await.unwrap();
        assert_eq!(ack.resource, ResourceId("song".into()));
        assert_eq!(ack.grant, g.seal.sum);
        assert_eq!(ack.fence, Fence::Epoch(Epoch(1)));
        assert!(ack.durability >= level);
        assert_eq!(
            s.installed_fence(&l, &t).await.unwrap(),
            Some(Fence::Epoch(Epoch(1)))
        );

        let new = c.claim(&l, &t, Epoch(1), Some(g.seal.sum));
        assert!(
            matches!(
                s.append_fenced(&new, level).await,
                Err(FenceError::BadGrant)
            ),
            "not activated"
        );
        s.append(&c.activate(&g), level).await.unwrap();
        assert!(
            matches!(s.append(&new, level).await, Err(StoreError::Route(_))),
            "append refuses fenced"
        );
        s.append_fenced(&new, level).await.unwrap();

        assert!(matches!(
            s.append_fenced(&old, level).await,
            Err(FenceError::Stale)
        ));
        s.archive(&old, level).await.unwrap();
        s.append(&old, level).await.unwrap();
        assert_eq!(
            s.get(&old.seal.sum).await.unwrap().unwrap().class,
            RecordClass::History
        );
    }

    /// Installing an older grant after a newer one leaves the newer fence in force.
    pub async fn a_fence_never_lowers(s: &dyn SongStore, _level: Durability) {
        let mut c = Chorus::new(1);
        let (l, t) = (lane(0), task("T-mono"));
        let g1 = c.grant(&l, &t, Epoch(1));
        let g2 = c.grant(&l, &t, Epoch(2));
        s.install_fence(&g2).await.unwrap();
        let ack = s.install_fence(&g1).await.unwrap();
        assert_eq!(ack.fence, Fence::Epoch(Epoch(2)));
        assert_eq!(
            s.installed_fence(&l, &t).await.unwrap(),
            Some(Fence::Epoch(Epoch(2)))
        );
    }

    /// `record` needs a control kind on a closed task; after closure work is refused and late
    /// verses archive.
    pub async fn closure_routes_control(s: &dyn SongStore, level: Durability) {
        let mut c = Chorus::new(1);
        let (l, t) = (lane(0), task("T-close"));
        s.append(&c.claim(&l, &t, Epoch(0), None), level)
            .await
            .unwrap();
        let snap = s.put_blob(b"snapshot").await.unwrap();
        let close = c.close(&t, snap);
        assert!(
            matches!(s.record(&close, level).await, Err(StoreError::Route(_))),
            "task still open"
        );
        let work = c.work(&l, &t, Epoch(0), None);
        assert!(
            matches!(s.record(&work, level).await, Err(StoreError::Route(_))),
            "not a control kind"
        );

        let ack = s.install_fence(&close).await.unwrap();
        assert_eq!(ack.fence, Fence::Closed);
        assert_eq!(s.task_state(&t).await.unwrap(), TaskState::Closed);
        s.record(&close, level).await.unwrap();
        assert_eq!(
            s.get(&close.seal.sum).await.unwrap().unwrap().class,
            RecordClass::Control
        );
        assert!(
            matches!(s.append(&work, level).await, Err(StoreError::Route(_))),
            "closed"
        );
        s.archive(&work, level).await.unwrap();
        assert_eq!(
            s.get(&work.seal.sum).await.unwrap().unwrap().class,
            RecordClass::History
        );
        assert_eq!(
            s.task_state(&task("T-other")).await.unwrap(),
            TaskState::Open
        );
    }

    /// `head` returns the highest epoch's last verse for one singer on one task only.
    pub async fn head_is_scoped_per_task(s: &dyn SongStore, level: Durability) {
        let mut c = Chorus::new(1);
        let l = lane(0);
        let (t1, t2) = (task("T-h1"), task("T-h2"));
        s.append(&c.claim(&l, &t1, Epoch(0), None), level)
            .await
            .unwrap();
        let last1 = c.work(&l, &t1, Epoch(0), None);
        s.append(&last1, level).await.unwrap();
        let only2 = c.claim(&l, &t2, Epoch(0), None);
        s.append(&only2, level).await.unwrap();
        let h1 = s.head(&l, &t1).await.unwrap().unwrap();
        assert_eq!((h1.epoch, h1.seq, h1.hash), (Epoch(0), 1, last1.seal.sum));
        assert_eq!(s.head(&l, &t2).await.unwrap().unwrap().hash, only2.seal.sum);
        assert!(
            s.head(&SingerId(ALLOCATOR.into()), &t1)
                .await
                .unwrap()
                .is_none()
        );
    }

    /// Blobs by blake3; only the newest refrain per chain; the scribe's reads.
    pub async fn blobs_refrains_and_scribe_reads(s: &dyn SongStore, level: Durability) {
        let h = s.put_blob(b"roster").await.unwrap();
        assert_eq!(h, digest(b"roster"));
        assert_eq!(
            s.get_blob(&h).await.unwrap().as_deref(),
            Some(&b"roster"[..])
        );
        assert_eq!(s.blob(&h).await.unwrap().as_deref(), Some(&b"roster"[..]));

        let mut c = Chorus::new(1);
        let (l, t) = (lane(0), task("T-scribe"));
        let claim = c.claim(&l, &t, Epoch(0), None);
        s.append(&claim, level).await.unwrap();
        for n in [2, 1, 3, 3] {
            s.refrain(&crate::refrain(&c, &l, &t, claim.seal.sum, n))
                .await
                .unwrap();
        }
        let latest = s.latest_refrains().await.unwrap();
        assert_eq!(latest.len(), 1);
        assert_eq!(latest[0].n, 3);

        let snap = s.put_blob(b"closure snapshot").await.unwrap();
        let close = c.close(&t, snap);
        s.install_fence(&close).await.unwrap();
        s.record(&close, level).await.unwrap();
        let late = c.work(&l, &t, Epoch(0), None);
        s.archive(&late, level).await.unwrap();
        assert_eq!(
            s.snapshot(&close.seal.sum).await.unwrap().as_deref(),
            Some(&b"closure snapshot"[..])
        );
        let classes: Vec<_> = s
            .task_records(&t)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.class)
            .collect();
        assert_eq!(
            classes,
            [
                RecordClass::Current,
                RecordClass::Control,
                RecordClass::History
            ]
        );
    }

    /// Core backends never claim replication; a request for it is refused, not downgraded.
    pub async fn durability_is_honoured_or_refused(s: &dyn SongStore, level: Durability) {
        let mut c = Chorus::new(1);
        let v = c.claim(&lane(0), &task("T-dur"), Epoch(0), None);
        let refused = s.append(&v, Durability::Replicated(2)).await;
        assert!(matches!(
            refused,
            Err(StoreError::Unsupported(Durability::Replicated(2)))
        ));
        assert!(
            s.get(&v.seal.sum).await.unwrap().is_none(),
            "nothing written"
        );
        assert!(s.append(&v, level).await.unwrap().level >= level);
    }
}

/// Run the `FencedResource` conformance suite: persisted, monotonic, never lowered, restart-safe;
/// every ack names the resource, the authorizing verse, the fence now enforced and the durability
/// reached; `Closed` installs through the same path as an epoch.
#[macro_export]
macro_rules! fenced_resource_conformance {
    ($name:ident, $make:expr) => {
        #[tokio::test]
        async fn $name() {
            let _resource = $make;
            // milestone 5
        }
    };
}

/// Run the `CadenceSource` conformance suite: a promise per state, version carried, fallback below ten.
#[macro_export]
macro_rules! cadence_source_conformance {
    ($name:ident, $make:expr) => {
        #[test]
        fn $name() {
            let _source = $make;
            // milestone 4
        }
    };
}
