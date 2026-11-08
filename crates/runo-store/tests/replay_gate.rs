//! The milestone-2 gate (technical spec §14): a chain written through the store service is
//! readable by the replay kit. Writes a chorus through `runo store serve`'s own code path — a
//! jsonl store behind the service, a client over a Unix socket — with a relaunch, an archived
//! old write, a fork and a closure, then runs `runo-replay/read-song.py` on the directory.

#![allow(
    clippy::many_single_char_names,
    reason = "store, chorus and lanes read better short in tests"
)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use runo_core::fence::Durability;
use runo_core::{Epoch, ResourceId, TaskId};
use runo_store::jsonl::JsonlStore;
use runo_store::service::{StoreClient, bind_unix, serve_unix};
use runo_store::{ResourceFenceStore, VerseWriter};
use runo_testkit::fixture::{Chorus, lane};

fn kit() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runo-replay/read-song.py")
}

#[tokio::test]
async fn a_chain_written_through_the_service_is_readable_by_the_replay_kit() {
    let dir = std::env::temp_dir().join(format!("runo-gate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = JsonlStore::open(&dir, ResourceId("song".into()), Durability::Host).unwrap();
    let sock = dir.join("store.sock");
    tokio::spawn(serve_unix(Arc::new(store), bind_unix(&sock).unwrap()));
    let s = StoreClient::connect_unix(&sock).await.unwrap();
    let host = Durability::Host;

    let mut c = Chorus::new(2);
    let (a, b) = (lane(0), lane(1));
    let (t1, t2) = (TaskId("HOD-42".into()), TaskId("HOD-43".into()));

    // lane-0 works HOD-42, stalls, and is relaunched at epoch 1; its paused write lands late.
    s.append(&c.claim(&a, &t1, Epoch(0), None), host)
        .await
        .unwrap();
    s.append(&c.work(&a, &t1, Epoch(0), None), host)
        .await
        .unwrap();
    let paused = c.work(&a, &t1, Epoch(0), None);
    let g = c.grant(&a, &t1, Epoch(1));
    s.append(&g, host).await.unwrap();
    s.install_fence(&g).await.unwrap();
    s.append(&c.activate(&g), host).await.unwrap();
    s.append_fenced(&c.claim(&a, &t1, Epoch(1), Some(g.seal.sum)), host)
        .await
        .unwrap();
    s.append_fenced(&c.work(&a, &t1, Epoch(1), Some(g.seal.sum)), host)
        .await
        .unwrap();
    s.archive(&paused, host).await.unwrap();

    // lane-1 works HOD-43 and equivocates at seq 1; lane-0 acks its claim.
    let claim_b = c.claim(&b, &t2, Epoch(0), None);
    s.append(&claim_b, host).await.unwrap();
    let one = c.work(&b, &t2, Epoch(0), None);
    let mut other = one.clone();
    other.header.note = Some("the other variant".into());
    let other = runo_core::sign::seal(c.key(&b), other.header, other.payload).unwrap();
    s.append(&one, host).await.unwrap();
    s.append(&other, host).await.unwrap();
    s.append(&c.ack(&a, &t2, &claim_b), host).await.unwrap();

    // HOD-43 closes; a late verse is archived after it.
    let snap = s.put_blob(b"closure snapshot of HOD-43").await.unwrap();
    let closing = c.closing(&t2);
    s.append(&closing, host).await.unwrap();
    let close = c.close(&t2, snap);
    s.install_fence(&closing).await.unwrap();
    s.record(&close, host).await.unwrap();
    s.archive(&c.work(&b, &t2, Epoch(0), None), host)
        .await
        .unwrap();

    let csv = dir.join("chains.csv");
    let out = Command::new("python3")
        .arg(kit())
        .arg(&dir)
        .arg("--csv")
        .arg(&csv)
        .output()
        .expect("python3 runs read-song.py");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "read-song.py failed:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.starts_with("14 records in 6 chains, 2 tasks, 3 singers"),
        "{stdout}"
    );
    assert!(stdout.contains("fork: lane-1"), "{stdout}");
    assert!(!stdout.contains("BAD"), "{stdout}");
    let rows = std::fs::read_to_string(&csv).unwrap();
    assert!(
        rows.contains("lane-0,HOD-42,0,3,2,1,0,"),
        "the paused write is History:\n{rows}"
    );
    assert!(rows.contains("lane-0,HOD-42,1,2,2,0,0,"), "{rows}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_kit_refuses_a_broken_chain() {
    let dir = std::env::temp_dir().join(format!("runo-gate-bad-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut c = Chorus::new(1);
    let t = TaskId("HOD-44".into());
    let _skipped = c.claim(&lane(0), &t, Epoch(0), None);
    let orphan = c.work(&lane(0), &t, Epoch(0), None);
    let line = serde_json::json!({ "class": "current", "verse": orphan });
    std::fs::write(dir.join("song.jsonl"), format!("{line}\n")).unwrap();
    let out = Command::new("python3")
        .arg(kit())
        .arg(&dir)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let _ = std::fs::remove_dir_all(&dir);
}
