//! The allocator's journal (technical spec §8; technical decisions 5): an append-only list of
//! events, folded into [`JournalState`]. [`MemJournal`] keeps it in memory (tests, the simulator);
//! [`JsonlJournal`] writes one JSON line per event and `fsync`s before answering, so a crashed
//! allocator resumes from the recorded step and never reuses an epoch.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use runo_core::ingress::TaskState;
use runo_core::{Epoch, Hash, SingerId, TaskId, VerseId};
use serde::{Deserialize, Serialize};

use crate::{AllocatorJournal, FenceError, Reservation};

/// One journal line.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    /// An epoch was reserved for `(singer, task)`.
    Reserved {
        /// The holder.
        singer: SingerId,
        /// The task.
        task: TaskId,
        /// The epoch.
        epoch: Epoch,
    },
    /// The reservation's `Grant` verse was sung.
    Granted {
        /// The holder.
        singer: SingerId,
        /// The task.
        task: TaskId,
        /// The grant.
        grant: VerseId,
    },
    /// The grant was activated; the reservation is complete.
    Activated {
        /// The holder.
        singer: SingerId,
        /// The task.
        task: TaskId,
        /// The grant.
        grant: VerseId,
    },
    /// A reservation was abandoned during closure; its epoch is never reused.
    Abandoned {
        /// The holder.
        singer: SingerId,
        /// The task.
        task: TaskId,
        /// The epoch.
        epoch: Epoch,
    },
    /// The task moved to `Closing` (with its frozen grant set and authorizing verse) or `Closed`.
    Lifecycle {
        /// The task.
        task: TaskId,
        /// The new state.
        state: TaskState,
        /// The frozen grant set, when `Closing`.
        grants: Vec<VerseId>,
        /// The `Closing` verse, once sung.
        authority: Option<VerseId>,
    },
    /// The closure snapshot's hash, persisted before `Close` is sung. The first one wins.
    Snapshot {
        /// The task.
        task: TaskId,
        /// The snapshot blob's hash.
        hash: Hash,
    },
}

/// What a reservation is doing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Reserved,
    Granted(VerseId),
    Done,
}

/// One task's lifecycle as the allocator knows it.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Lifecycle {
    /// Open, closing or closed.
    pub state: Option<TaskState>,
    /// The frozen grant set.
    pub grants: Vec<VerseId>,
    /// The `Closing` verse.
    pub authority: Option<VerseId>,
    /// The persisted snapshot hash.
    pub snapshot: Option<Hash>,
}

/// The journal folded: what an allocator needs to resume.
#[derive(Clone, Debug, Default)]
pub struct JournalState {
    /// The highest epoch ever reserved per chain owner, abandoned ones included.
    highest: BTreeMap<(SingerId, TaskId), Epoch>,
    /// The reservation in flight per chain owner, if any.
    pending: BTreeMap<(SingerId, TaskId), (Epoch, Phase)>,
    /// Every grant ever sung per task, with its holder and epoch.
    grants: BTreeMap<TaskId, Vec<(SingerId, Epoch, VerseId)>>,
    /// Task lifecycles.
    tasks: BTreeMap<TaskId, Lifecycle>,
}

impl JournalState {
    /// Fold one event in. Idempotent for a repeated event.
    pub fn apply(&mut self, e: &Event) {
        match e {
            Event::Reserved {
                singer,
                task,
                epoch,
            } => {
                let key = (singer.clone(), task.clone());
                let high = self.highest.entry(key.clone()).or_insert(*epoch);
                *high = (*high).max(*epoch);
                self.pending.insert(key, (*epoch, Phase::Reserved));
            }
            Event::Granted {
                singer,
                task,
                grant,
            } => {
                let key = (singer.clone(), task.clone());
                if let Some((epoch, phase)) = self.pending.get_mut(&key) {
                    *phase = Phase::Granted(*grant);
                    let list = self.grants.entry(task.clone()).or_default();
                    if !list.iter().any(|(_, _, g)| g == grant) {
                        list.push((singer.clone(), *epoch, *grant));
                    }
                }
            }
            Event::Activated { singer, task, .. } => {
                if let Some((_, phase)) = self.pending.get_mut(&(singer.clone(), task.clone())) {
                    *phase = Phase::Done;
                }
            }
            Event::Abandoned { singer, task, .. } => {
                self.pending.remove(&(singer.clone(), task.clone()));
            }
            Event::Lifecycle {
                task,
                state,
                grants,
                authority,
            } => {
                let l = self.tasks.entry(task.clone()).or_default();
                l.state = Some(*state);
                if !grants.is_empty() {
                    l.grants.clone_from(grants);
                }
                if authority.is_some() {
                    l.authority = *authority;
                }
            }
            Event::Snapshot { task, hash } => {
                let l = self.tasks.entry(task.clone()).or_default();
                l.snapshot.get_or_insert(*hash);
            }
        }
    }

    fn reserve(
        &self,
        singer: &SingerId,
        task: &TaskId,
    ) -> Result<(Reservation, Option<Event>), FenceError> {
        let key = (singer.clone(), task.clone());
        match self.pending.get(&key) {
            Some((epoch, Phase::Reserved)) => Ok((
                Reservation {
                    epoch: *epoch,
                    grant: None,
                },
                None,
            )),
            Some((epoch, Phase::Granted(g))) => Ok((
                Reservation {
                    epoch: *epoch,
                    grant: Some(*g),
                },
                None,
            )),
            _ => {
                if self
                    .lifecycle(task)
                    .state
                    .is_some_and(|s| s != TaskState::Open)
                {
                    return Err(FenceError::Closing);
                }
                let next = Epoch(self.highest.get(&key).map_or(1, |e| e.0 + 1));
                Ok((
                    Reservation {
                        epoch: next,
                        grant: None,
                    },
                    Some(Event::Reserved {
                        singer: singer.clone(),
                        task: task.clone(),
                        epoch: next,
                    }),
                ))
            }
        }
    }

    /// The task's lifecycle.
    #[must_use]
    pub fn lifecycle(&self, task: &TaskId) -> Lifecycle {
        self.tasks.get(task).cloned().unwrap_or_default()
    }

    /// Every grant ever sung for the task: holder, epoch, grant.
    #[must_use]
    pub fn grants(&self, task: &TaskId) -> Vec<(SingerId, Epoch, VerseId)> {
        self.grants.get(task).cloned().unwrap_or_default()
    }

    /// Reservations on the task that never became a grant.
    #[must_use]
    pub fn ungranted(&self, task: &TaskId) -> Vec<(SingerId, Epoch)> {
        self.pending
            .iter()
            .filter(|((_, t), (_, p))| t == task && *p == Phase::Reserved)
            .map(|((s, _), (e, _))| (s.clone(), *e))
            .collect()
    }
}

/// Shared by both journals: fold under a lock, persist through `write` before applying.
struct Folded {
    state: JournalState,
}

impl Folded {
    fn record(
        &mut self,
        events: impl IntoIterator<Item = Event>,
        write: &mut Write<'_>,
    ) -> Result<(), FenceError> {
        for e in events {
            write(&e)?;
            self.state.apply(&e);
        }
        Ok(())
    }
}

/// Writes one event durably, before it is applied.
type Write<'w> = dyn FnMut(&Event) -> Result<(), FenceError> + 'w;
/// What a backend runs under its lock: the folded state and the writer.
type Step<'s> = dyn FnMut(&mut Folded, &mut Write<'_>) -> Result<(), FenceError> + 's;

/// The parts of the journal API both backends implement the same way.
trait Backend: Send + Sync {
    fn with(&self, f: &mut Step<'_>) -> Result<(), FenceError>;
}

/// A journal in memory: the simulator's and the tests'.
#[derive(Default)]
pub struct MemJournal {
    inner: Mutex<Option<Folded>>,
    log: Mutex<Vec<Event>>,
}

impl MemJournal {
    /// An empty journal.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every event so far, for a test to replay into a fresh journal (a simulated restart).
    #[must_use]
    pub fn events(&self) -> Vec<Event> {
        self.log.lock().map(|l| l.clone()).unwrap_or_default()
    }

    /// A journal that has already seen `events`: an allocator restarting from them.
    #[must_use]
    pub fn from_events(events: &[Event]) -> Self {
        let mut state = JournalState::default();
        for e in events {
            state.apply(e);
        }
        Self {
            inner: Mutex::new(Some(Folded { state })),
            log: Mutex::new(events.to_vec()),
        }
    }
}

impl Backend for MemJournal {
    fn with(&self, f: &mut Step<'_>) -> Result<(), FenceError> {
        let mut inner = self.inner.lock().map_err(|_| poisoned())?;
        let folded = inner.get_or_insert_with(|| Folded {
            state: JournalState::default(),
        });
        let mut write = |e: &Event| {
            self.log.lock().map_err(|_| poisoned())?.push(e.clone());
            Ok(())
        };
        f(folded, &mut write)
    }
}

/// A journal in a file, one JSON event per line, `fsync`ed before each answer.
pub struct JsonlJournal {
    path: PathBuf,
    inner: Mutex<(Folded, File)>,
}

impl JsonlJournal {
    /// Open (or create) the journal at `path` and fold what it holds. A torn last line from a
    /// crash mid-write is cut off; any other unreadable line refuses the open.
    ///
    /// # Errors
    /// The file cannot be read or holds a malformed line.
    pub fn open(path: &Path) -> Result<Self, FenceError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| io(path, &e))?;
        }
        let mut state = JournalState::default();
        let mut good = 0u64;
        if path.exists() {
            let f = File::open(path).map_err(|e| io(path, &e))?;
            let lines: Vec<String> = BufReader::new(f)
                .lines()
                .collect::<Result<_, _>>()
                .map_err(|e| io(path, &e))?;
            let last = lines.len().saturating_sub(1);
            for (i, line) in lines.iter().enumerate() {
                match serde_json::from_str::<Event>(line) {
                    Ok(e) => {
                        state.apply(&e);
                        good += line.len() as u64 + 1;
                    }
                    Err(_) if i == last => break, // torn by a crash: never answered, so never used
                    Err(e) => {
                        return Err(FenceError::Journal(format!(
                            "{} line {}: {e}",
                            path.display(),
                            i + 1
                        )));
                    }
                }
            }
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| io(path, &e))?;
        file.set_len(good).map_err(|e| io(path, &e))?;
        Ok(Self {
            path: path.to_owned(),
            inner: Mutex::new((Folded { state }, file)),
        })
    }
}

impl JsonlJournal {
    /// Where the journal lives.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Backend for JsonlJournal {
    fn with(&self, f: &mut Step<'_>) -> Result<(), FenceError> {
        let mut inner = self.inner.lock().map_err(|_| poisoned())?;
        let (folded, file) = &mut *inner;
        let path = &self.path;
        let mut write = |e: &Event| {
            let mut line =
                serde_json::to_string(e).map_err(|err| FenceError::Journal(err.to_string()))?;
            line.push('\n');
            file.write_all(line.as_bytes())
                .map_err(|err| io(path, &err))?;
            file.sync_data().map_err(|err| io(path, &err))
        };
        f(folded, &mut write)
    }
}

fn poisoned() -> FenceError {
    FenceError::Journal("journal lock poisoned".into())
}

fn io(path: &Path, e: &std::io::Error) -> FenceError {
    FenceError::Journal(format!("{}: {e}", path.display()))
}

fn state_of(b: &dyn Backend) -> Result<JournalState, FenceError> {
    let mut out = None;
    b.with(&mut |f, _| {
        out = Some(f.state.clone());
        Ok(())
    })?;
    out.ok_or_else(poisoned)
}

fn reserve_in(
    b: &dyn Backend,
    singer: &SingerId,
    task: &TaskId,
) -> Result<Reservation, FenceError> {
    let mut out = None;
    b.with(&mut |f, w| {
        let (r, event) = f.state.reserve(singer, task)?;
        f.record(event, w)?;
        out = Some(r);
        Ok(())
    })?;
    out.ok_or_else(poisoned)
}

fn record_in(b: &dyn Backend, e: &Event) -> Result<(), FenceError> {
    b.with(&mut |f, w| f.record([e.clone()], w))
}

fn snapshot_in(b: &dyn Backend, task: &TaskId, hash: Hash) -> Result<Hash, FenceError> {
    let mut out = None;
    b.with(&mut |f, w| {
        if let Some(h) = f.state.lifecycle(task).snapshot {
            out = Some(h);
            return Ok(());
        }
        f.record(
            [Event::Snapshot {
                task: task.clone(),
                hash,
            }],
            w,
        )?;
        out = Some(hash);
        Ok(())
    })?;
    out.ok_or_else(poisoned)
}

macro_rules! journal {
    ($t:ty) => {
        #[async_trait]
        impl AllocatorJournal for $t {
            async fn reserve(
                &self,
                singer: &SingerId,
                task: &TaskId,
            ) -> Result<Reservation, FenceError> {
                reserve_in(self, singer, task)
            }

            async fn granted(
                &self,
                singer: &SingerId,
                task: &TaskId,
                grant: VerseId,
            ) -> Result<(), FenceError> {
                let e = Event::Granted {
                    singer: singer.clone(),
                    task: task.clone(),
                    grant,
                };
                record_in(self, &e)
            }

            async fn activated(
                &self,
                singer: &SingerId,
                task: &TaskId,
                grant: VerseId,
            ) -> Result<(), FenceError> {
                let e = Event::Activated {
                    singer: singer.clone(),
                    task: task.clone(),
                    grant,
                };
                record_in(self, &e)
            }

            async fn abandoned(
                &self,
                singer: &SingerId,
                task: &TaskId,
                epoch: Epoch,
            ) -> Result<(), FenceError> {
                let e = Event::Abandoned {
                    singer: singer.clone(),
                    task: task.clone(),
                    epoch,
                };
                record_in(self, &e)
            }

            async fn task_state(&self, task: &TaskId) -> Result<TaskState, FenceError> {
                Ok(state_of(self)?
                    .lifecycle(task)
                    .state
                    .unwrap_or(TaskState::Open))
            }

            async fn set_task_state(
                &self,
                task: &TaskId,
                state: TaskState,
                grants: &[VerseId],
                authority: Option<VerseId>,
            ) -> Result<(), FenceError> {
                let e = Event::Lifecycle {
                    task: task.clone(),
                    state,
                    grants: grants.to_vec(),
                    authority,
                };
                record_in(self, &e)
            }

            async fn closure_snapshot(
                &self,
                task: &TaskId,
                hash: Hash,
            ) -> Result<Hash, FenceError> {
                snapshot_in(self, task, hash)
            }

            fn read(&self) -> Result<JournalState, FenceError> {
                state_of(self)
            }
        }
    };
}

journal!(MemJournal);
journal!(JsonlJournal);
