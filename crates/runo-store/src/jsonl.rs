//! The jsonl backend (technical spec §7): one directory per chorus, human-readable and
//! grep-able, durable at `Host`.
//!
//! | Path | Content |
//! | --- | --- |
//! | `song.jsonl` | One record per line, in arrival order: `{"class": "current" \| "history" \| "control", "verse": …}`. The line's position is the record's [`Cursor`](crate::Cursor). |
//! | `fences.jsonl` | One line per fence change: `{"op": "fence", singer, task, epoch, grant}` or `{"op": "close", task, close}`; the highest fence per key wins on load. |
//! | `blobs/<hex>` | Roster documents, snapshots, tome manifests, by blake3. |
//! | `refrains/<singer>/<task>.json` | The latest refrain for that chain owner, replaced atomically. |
//! | `lock` | Held with an OS file lock while open; a second opener is refused, and a crash releases it. |
//!
//! The index is rebuilt in memory from the files on open (it is derived, so it is not written).
//! Every write runs inside one lock: check against the shared [`State`], write the line, `fsync`
//! at `Host`, then apply — the atomic step `append_fenced` needs. A torn last line left by a
//! crash mid-write is cut off on open; any other unreadable line refuses the open.

use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use async_trait::async_trait;
use runo_core::fence::{Durability, Fence, FenceAck};
use runo_core::hash::display;
use runo_core::ingress::{IngressView, RecordClass, TaskState};
use runo_core::{ChainKey, Epoch, Hash, Refrain, ResourceId, SingerId, TaskId, Verse, VerseId};
use serde::{Deserialize, Serialize};

use crate::state::{Change, Prepared, State};
use crate::{
    Cursor, FenceError, Head, Receipt, Record, RefrainSlot, ResourceFenceStore, ScribeRead,
    StoreError, VerseReader, VerseWriter,
};

/// One line of `song.jsonl`: the class beside the untouched verse.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SongLine {
    class: RecordClass,
    verse: Verse,
}

struct Files {
    song: File,
    fences: File,
}

struct Inner {
    state: State,
    files: Files,
}

/// The jsonl store.
pub struct JsonlStore {
    dir: PathBuf,
    resource: ResourceId,
    level: Durability,
    inner: Mutex<Inner>,
    _lock: File,
}

fn io(e: impl std::fmt::Display) -> StoreError {
    StoreError::Io(e.to_string())
}

impl JsonlStore {
    /// Open (creating if needed) the chorus directory `dir`, naming this store `resource` in
    /// fence acks, durable at `level` (`Process` or `Host`).
    ///
    /// # Errors
    /// `Unsupported` above `Host`; `Io` if the directory is held by another process, or a file
    /// is unreadable other than a torn last line.
    pub fn open(dir: &Path, resource: ResourceId, level: Durability) -> Result<Self, StoreError> {
        if level > Durability::Host {
            return Err(StoreError::Unsupported(level));
        }
        fs::create_dir_all(dir.join("blobs")).map_err(io)?;
        fs::create_dir_all(dir.join("refrains")).map_err(io)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("lock"))
            .map_err(io)?;
        lock.try_lock()
            .map_err(|_| StoreError::Io(format!("{} is open in another process", dir.display())))?;

        let mut state = State::default();
        let song = load_lines::<SongLine>(&dir.join("song.jsonl"), |l| {
            state.apply(Change::Record {
                class: l.class,
                verse: Box::new(l.verse),
            });
        })?;
        let fences = load_lines::<Change>(&dir.join("fences.jsonl"), |c| state.apply(c))?;
        for entry in fs::read_dir(dir.join("blobs")).map_err(io)? {
            let path = entry.map_err(io)?.path();
            if path.extension().is_some_and(|e| e == "tmp") {
                continue;
            }
            let bytes = fs::read(&path).map_err(io)?;
            let (hash, change) = state.prepare_blob(&bytes);
            if display(&hash).strip_prefix("b3:") != path.file_name().and_then(|n| n.to_str()) {
                return Err(StoreError::Io(format!(
                    "blob {} does not match its name",
                    path.display()
                )));
            }
            if let Some(c) = change {
                state.apply(c);
            }
        }
        for path in walk(&dir.join("refrains"))? {
            if path.extension().is_some_and(|e| e == "json") {
                let r: Refrain =
                    serde_json::from_slice(&fs::read(&path).map_err(io)?).map_err(io)?;
                state.apply(Change::Refrain {
                    refrain: Box::new(r),
                });
            }
        }
        if level == Durability::Host {
            sync_dir(dir)?;
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            resource,
            level,
            inner: Mutex::new(Inner {
                state,
                files: Files { song, fences },
            }),
            _lock: lock,
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn check(&self, level: Durability) -> Result<(), StoreError> {
        if level > self.level {
            return Err(StoreError::Unsupported(level));
        }
        Ok(())
    }

    /// Persist then apply, inside the caller's lock.
    fn commit(
        &self,
        inner: &mut Inner,
        change: Change,
        level: Durability,
    ) -> Result<(), StoreError> {
        match &change {
            Change::Record { class, verse } => {
                let line = SongLine {
                    class: *class,
                    verse: (**verse).clone(),
                };
                append_line(&mut inner.files.song, &line, level)?;
            }
            Change::Fence { .. } | Change::Close { .. } => {
                // Fences are durable at Host before they are acknowledged, whatever was asked.
                append_line(&mut inner.files.fences, &change, Durability::Host)?;
            }
            Change::Blob { hash, bytes } => {
                let name = display(hash);
                let path = self.dir.join("blobs").join(&name[3..]);
                write_atomically(&path, bytes, level)?;
            }
            Change::Refrain { refrain } => {
                let path = self
                    .dir
                    .join("refrains")
                    .join(safe(&refrain.singer.0))
                    .join(format!("{}.json", safe(&refrain.task.0)));
                let bytes = serde_json::to_vec(refrain).map_err(io)?;
                write_atomically(&path, &bytes, Durability::Process)?;
            }
        }
        inner.state.apply(change);
        Ok(())
    }

    fn write(
        &self,
        v: &Verse,
        level: Durability,
        prepared: Prepared<()>,
        mut inner: MutexGuard<'_, Inner>,
    ) -> Result<Receipt, StoreError> {
        if let Prepared::Change(c, ()) = prepared {
            self.commit(&mut inner, c, level)?;
        }
        Ok(Receipt {
            id: v.seal.sum,
            level,
        })
    }
}

fn append_line<T: Serialize>(
    file: &mut File,
    value: &T,
    level: Durability,
) -> Result<(), StoreError> {
    let mut line = serde_json::to_vec(value).map_err(io)?;
    line.push(b'\n');
    file.write_all(&line).map_err(io)?;
    if level >= Durability::Host {
        file.sync_data().map_err(io)?;
    }
    Ok(())
}

fn write_atomically(path: &Path, bytes: &[u8], level: Durability) -> Result<(), StoreError> {
    let parent = path.parent().ok_or_else(|| io("path has no parent"))?;
    fs::create_dir_all(parent).map_err(io)?;
    let tmp = path.with_extension("tmp");
    let mut f = File::create(&tmp).map_err(io)?;
    f.write_all(bytes).map_err(io)?;
    if level >= Durability::Host {
        f.sync_all().map_err(io)?;
    }
    fs::rename(&tmp, path).map_err(io)?;
    if level >= Durability::Host {
        sync_dir(parent)?;
    }
    Ok(())
}

fn sync_dir(dir: &Path) -> Result<(), StoreError> {
    File::open(dir).and_then(|d| d.sync_all()).map_err(io)
}

/// Read a jsonl file, calling `each` per line; cut off a torn last line; return the file opened
/// for appending.
fn load_lines<T: serde::de::DeserializeOwned>(
    path: &Path,
    mut each: impl FnMut(T),
) -> Result<File, StoreError> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(io(e)),
    };
    let complete = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    for (n, line) in bytes[..complete].split(|b| *b == b'\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        let value = serde_json::from_slice(line)
            .map_err(|e| StoreError::Io(format!("{} line {}: {e}", path.display(), n + 1)))?;
        each(value);
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(io)?;
    if complete < bytes.len() {
        file.set_len(complete as u64).map_err(io)?;
        file.sync_data().map_err(io)?;
    }
    Ok(file)
}

fn walk(dir: &Path) -> Result<Vec<PathBuf>, StoreError> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).map_err(io)? {
        let path = entry.map_err(io)?.path();
        if path.is_dir() {
            out.extend(walk(&path)?);
        } else {
            out.push(path);
        }
    }
    Ok(out)
}

/// A path segment from an id: ASCII letters, digits, `.`, `_` and `-` kept, anything else `_`.
fn safe(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[async_trait]
impl VerseReader for JsonlStore {
    async fn read_since(&self, cursor: Cursor) -> Result<(Vec<Record>, Cursor), StoreError> {
        Ok(self.lock().state.read_since(cursor))
    }
    async fn get(&self, id: &VerseId) -> Result<Option<Record>, StoreError> {
        Ok(self.lock().state.get(id))
    }
    async fn head(&self, singer: &SingerId, task: &TaskId) -> Result<Option<Head>, StoreError> {
        Ok(self.lock().state.head(singer, task))
    }
    async fn get_blob(&self, h: &Hash) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.lock().state.blob(h))
    }
}

#[async_trait]
impl VerseWriter for JsonlStore {
    async fn append(&self, v: &Verse, level: Durability) -> Result<Receipt, StoreError> {
        self.check(level)?;
        let inner = self.lock();
        let p = inner.state.prepare_append(v)?;
        self.write(v, level, p, inner)
    }
    async fn append_fenced(&self, v: &Verse, level: Durability) -> Result<Receipt, FenceError> {
        self.check(level)?;
        let inner = self.lock();
        let p = inner.state.prepare_append_fenced(v)?;
        Ok(self.write(v, level, p, inner)?)
    }
    async fn archive(&self, v: &Verse, level: Durability) -> Result<Receipt, StoreError> {
        self.check(level)?;
        let inner = self.lock();
        let p = inner.state.prepare_archive(v)?;
        self.write(v, level, p, inner)
    }
    async fn record(&self, v: &Verse, level: Durability) -> Result<Receipt, StoreError> {
        self.check(level)?;
        let inner = self.lock();
        let p = inner.state.prepare_record(v)?;
        self.write(v, level, p, inner)
    }
    async fn put_blob(&self, bytes: &[u8]) -> Result<Hash, StoreError> {
        let mut inner = self.lock();
        let (hash, change) = inner.state.prepare_blob(bytes);
        if let Some(c) = change {
            self.commit(&mut inner, c, self.level)?;
        }
        Ok(hash)
    }
}

#[async_trait]
impl ResourceFenceStore for JsonlStore {
    async fn install_fence(&self, grant: &Verse) -> Result<FenceAck, StoreError> {
        let mut inner = self.lock();
        let durability = Durability::Host.max(self.level);
        match inner
            .state
            .prepare_install(grant, &self.resource, durability)?
        {
            Prepared::Change(c, ack) => {
                self.commit(&mut inner, c, durability)?;
                Ok(ack)
            }
            Prepared::Stored(_) => unreachable!("install never dedups"),
        }
    }
    async fn installed_fence(
        &self,
        singer: &SingerId,
        task: &TaskId,
    ) -> Result<Option<Fence>, StoreError> {
        Ok(self.lock().state.installed_fence(singer, task))
    }
    async fn task_state(&self, task: &TaskId) -> Result<TaskState, StoreError> {
        Ok(self.lock().state.task_state(task))
    }
}

#[async_trait]
impl RefrainSlot for JsonlStore {
    async fn refrain(&self, r: &Refrain) -> Result<(), StoreError> {
        let mut inner = self.lock();
        if let Some(c) = inner.state.prepare_refrain(r) {
            self.commit(&mut inner, c, Durability::Process)?;
        }
        Ok(())
    }
    async fn latest_refrains(&self) -> Result<Vec<Refrain>, StoreError> {
        Ok(self.lock().state.latest_refrains())
    }
}

#[async_trait]
impl ScribeRead for JsonlStore {
    async fn snapshot(&self, close: &VerseId) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.lock().state.snapshot(close))
    }
    async fn blob(&self, h: &Hash) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.lock().state.blob(h))
    }
    async fn task_records(&self, task: &TaskId) -> Result<Vec<Record>, StoreError> {
        Ok(self.lock().state.task_records(task))
    }
}

impl IngressView for JsonlStore {
    fn stored_class(&self, id: &VerseId) -> Option<RecordClass> {
        self.lock().state.stored_class(id)
    }
    fn get(&self, id: &VerseId) -> Option<Verse> {
        IngressView::get(&self.lock().state, id)
    }
    fn variants(&self, key: &ChainKey, seq: u64) -> Vec<VerseId> {
        self.lock().state.variants(key, seq)
    }
    fn installed_fence(&self, singer: &SingerId, task: &TaskId) -> Option<Epoch> {
        IngressView::installed_fence(&self.lock().state, singer, task)
    }
    fn task_state(&self, task: &TaskId) -> TaskState {
        self.lock().state.task_state(task)
    }
    fn has_activation(&self, grant: &VerseId) -> bool {
        self.lock().state.has_activation(grant)
    }
}
