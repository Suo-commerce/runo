//! The jsonl backend against the store conformance suite, plus what only a file can get wrong:
//! reopening, a torn last line, and a second opener (technical spec §7).

#![allow(
    clippy::many_single_char_names,
    reason = "store, chorus, lane and task read better short in tests"
)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use runo_core::fence::{Durability, Fence};
use runo_core::ingress::RecordClass;
use runo_core::{Epoch, ResourceId, TaskId};
use runo_store::jsonl::JsonlStore;
use runo_store::{Cursor, ResourceFenceStore, StoreError, VerseReader, VerseWriter};
use runo_testkit::fixture::{Chorus, lane};

fn fresh_dir() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "runo-jsonl-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn open(dir: &std::path::Path) -> JsonlStore {
    JsonlStore::open(dir, ResourceId("song".into()), Durability::Host).unwrap()
}

runo_testkit::store_conformance!(jsonl, open(&fresh_dir()), Durability::Host);

#[tokio::test]
async fn everything_survives_a_reopen() {
    let dir = fresh_dir();
    let mut c = Chorus::new(1);
    let (l, t) = (lane(0), TaskId("T-reopen".into()));
    let old = c.work(&l, &t, Epoch(0), None);
    let g = c.grant(&l, &t, Epoch(1));
    let snap;
    {
        let s = open(&dir);
        s.append(&c.claim(&l, &t, Epoch(0), None), Durability::Host)
            .await
            .unwrap();
        s.append(&g, Durability::Host).await.unwrap();
        s.install_fence(&g).await.unwrap();
        s.archive(&old, Durability::Host).await.unwrap();
        snap = s.put_blob(b"kept").await.unwrap();
    }
    let s = open(&dir);
    let (all, _) = s.read_since(Cursor::default()).await.unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all[2].class, RecordClass::History);
    assert_eq!(
        s.installed_fence(&l, &t).await.unwrap(),
        Some(Fence::Epoch(Epoch(1)))
    );
    assert_eq!(
        s.get_blob(&snap).await.unwrap().as_deref(),
        Some(&b"kept"[..])
    );
    let song = std::fs::read_to_string(dir.join("song.jsonl")).unwrap();
    assert!(
        song.lines().all(|l| l.starts_with("{\"class\":")),
        "one wrapper object per line"
    );
}

#[tokio::test]
async fn a_torn_last_line_is_cut_off_and_the_rest_kept() {
    let dir = fresh_dir();
    let mut c = Chorus::new(1);
    let t = TaskId("T-torn".into());
    {
        let s = open(&dir);
        s.append(&c.claim(&lane(0), &t, Epoch(0), None), Durability::Host)
            .await
            .unwrap();
    }
    let path = dir.join("song.jsonl");
    let mut bytes = std::fs::read(&path).unwrap();
    let whole = bytes.len();
    bytes.extend_from_slice(b"{\"class\":\"current\",\"ver");
    std::fs::write(&path, &bytes).unwrap();

    let s = open(&dir);
    assert_eq!(s.read_since(Cursor::default()).await.unwrap().0.len(), 1);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), whole as u64);
    s.append(&c.work(&lane(0), &t, Epoch(0), None), Durability::Host)
        .await
        .unwrap();
    drop(s);
    assert_eq!(
        open(&dir)
            .read_since(Cursor::default())
            .await
            .unwrap()
            .0
            .len(),
        2
    );
}

#[test]
fn a_second_opener_is_refused_and_the_lock_goes_with_the_first() {
    let dir = fresh_dir();
    let first = open(&dir);
    let second = JsonlStore::open(&dir, ResourceId("song".into()), Durability::Host);
    assert!(matches!(second, Err(StoreError::Io(_))));
    drop(first);
    open(&dir);
}

#[test]
fn replication_is_refused_at_open() {
    let refused = JsonlStore::open(
        &fresh_dir(),
        ResourceId("song".into()),
        Durability::Replicated(2),
    );
    assert!(matches!(refused, Err(StoreError::Unsupported(_))));
}
