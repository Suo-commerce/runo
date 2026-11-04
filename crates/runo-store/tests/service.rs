//! The store conformance suite run through the service: a client over a Unix socket, with
//! `MemStore` and the jsonl backend behind it, plus TCP and a malformed request (technical
//! spec §7, "Ownership").

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use runo_core::fence::Durability;
use runo_core::{Epoch, ResourceId, TaskId};
use runo_store::jsonl::JsonlStore;
use runo_store::mem::MemStore;
use runo_store::service::{StoreClient, bind_unix, serve_tcp, serve_unix};
use runo_store::{Cursor, SongStore, VerseReader, VerseWriter};
use runo_testkit::fixture::{Chorus, lane};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

fn scratch(name: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("runo-svc-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

async fn served(store: Arc<dyn SongStore>) -> StoreClient {
    let sock = scratch("store.sock");
    let listener = bind_unix(&sock).unwrap();
    tokio::spawn(serve_unix(store, listener));
    StoreClient::connect_unix(&sock).await.unwrap()
}

async fn mem_client() -> StoreClient {
    served(Arc::new(MemStore::new())).await
}

async fn jsonl_client() -> StoreClient {
    let dir = scratch("chorus");
    let store = JsonlStore::open(&dir, ResourceId("song".into()), Durability::Host).unwrap();
    served(Arc::new(store)).await
}

runo_testkit::store_conformance!(through_service_mem, mem_client().await, Durability::Process);
runo_testkit::store_conformance!(
    through_service_jsonl,
    jsonl_client().await,
    Durability::Host
);

#[tokio::test]
async fn tcp_carries_the_same_protocol() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(serve_tcp(Arc::new(MemStore::new()), listener));
    let client = StoreClient::connect_tcp(&addr).await.unwrap();
    let mut c = Chorus::new(1);
    let v = c.claim(&lane(0), &TaskId("T-tcp".into()), Epoch(0), None);
    client.append(&v, Durability::Process).await.unwrap();
    assert_eq!(
        client.read_since(Cursor::default()).await.unwrap().0[0].verse,
        v
    );
}

#[tokio::test]
async fn a_malformed_request_gets_an_error_and_the_connection_stays_up() {
    let sock = scratch("store.sock");
    tokio::spawn(serve_unix(
        Arc::new(MemStore::new()),
        bind_unix(&sock).unwrap(),
    ));
    let stream = tokio::net::UnixStream::connect(&sock).await.unwrap();
    let (r, mut w) = stream.into_split();
    let mut lines = BufReader::new(r).lines();
    w.write_all(b"{\"call\":\"no_such_call\"}\n{\"call\":\"latest_refrains\"}\n")
        .await
        .unwrap();
    let first = lines.next_line().await.unwrap().unwrap();
    assert!(first.contains("\"error\":\"io\""), "{first}");
    let second = lines.next_line().await.unwrap().unwrap();
    assert!(second.contains("\"refrains\""), "{second}");
}
