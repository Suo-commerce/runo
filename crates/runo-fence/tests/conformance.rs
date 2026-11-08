//! The conformance suites from runo-testkit, run against this crate's implementations.

use std::path::PathBuf;
use std::sync::Arc;

use runo_core::fence::Durability;
use runo_core::{ResourceId, Verse};
use runo_fence::journal::{JsonlJournal, MemJournal};
use runo_fence::{FencedResource, SharedStoreResource};
use runo_store::SongStore;
use runo_store::jsonl::JsonlStore;
use runo_testkit::conformance::FenceHarness;

fn fresh(name: &str) -> PathBuf {
    let n = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "runo-fence-{name}-{}-{}",
        std::process::id(),
        n.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// The song store, on disk, as a fenced resource; a restart drops and reopens it.
struct Song {
    dir: PathBuf,
    res: Option<SharedStoreResource>,
}

impl Song {
    fn open(dir: PathBuf) -> Self {
        let store: Arc<dyn SongStore> =
            Arc::new(JsonlStore::open(&dir, ResourceId("song".into()), Durability::Host).unwrap());
        Self {
            dir,
            res: Some(SharedStoreResource {
                store,
                id: ResourceId("song".into()),
            }),
        }
    }
}

#[async_trait::async_trait]
impl FenceHarness for Song {
    fn resource(&self) -> &dyn FencedResource {
        self.res.as_ref().unwrap()
    }
    async fn publish(&self, v: &Verse) {
        self.res
            .as_ref()
            .unwrap()
            .store
            .append(v, Durability::Host)
            .await
            .unwrap();
    }
    async fn restart(&mut self) {
        self.res = None; // releases the store's lock
        *self = Song::open(self.dir.clone());
    }
}

runo_testkit::fenced_resource_conformance!(song_on_disk, Song::open(fresh("song")));

runo_testkit::journal_conformance!(mem_journal, MemJournal::new(), |j: MemJournal| {
    MemJournal::from_events(&j.events())
});

fn jsonl_path() -> PathBuf {
    fresh("journal").join("allocator.jsonl")
}

runo_testkit::journal_conformance!(
    jsonl_journal,
    {
        let p = jsonl_path();
        (JsonlJournal::open(&p).unwrap(), p)
    }
    .0,
    |j: JsonlJournal| {
        let p = j.path().to_owned();
        drop(j);
        JsonlJournal::open(&p).unwrap()
    }
);
