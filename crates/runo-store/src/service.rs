//! The store service and its client (technical spec §7, "Ownership"): one process owns the
//! backend and serves the role traits; nodes and CLI commands hold a [`StoreClient`] that
//! implements them over the transport. A Unix socket on the host, TCP on the chorus's private
//! network; the protocol is the same on both.
//!
//! **Protocol.** One JSON request per line, one JSON reply per line, in order, on one
//! connection. Each request is one role-trait call (`{"call": "append", "verse": …, "level":
//! …}`); the reply carries its result or the error the backend returned. Every call goes to the
//! backend, whose own lock makes `append_fenced` one critical section whatever the number of
//! clients.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use runo_core::fence::{Durability, Fence, FenceAck};
use runo_core::ingress::TaskState;
use runo_core::{Hash, Refrain, SingerId, TaskId, Verse, VerseId};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::Mutex;

use crate::{
    Cursor, FenceError, Head, Receipt, Record, RefrainSlot, ResourceFenceStore, ScribeRead,
    SongStore, StoreError, VerseReader, VerseWriter,
};

/// One role-trait call. Fields are that method's arguments, documented on the method.
#[allow(
    missing_docs,
    reason = "each field is the named trait method's argument"
)]
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "call", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// [`VerseReader::read_since`].
    ReadSince { cursor: Cursor },
    /// [`VerseReader::get`].
    Get { id: VerseId },
    /// [`VerseReader::head`].
    Head { singer: SingerId, task: TaskId },
    /// [`VerseReader::get_blob`].
    GetBlob { hash: Hash },
    /// [`VerseWriter::append`].
    Append {
        verse: Box<Verse>,
        level: Durability,
    },
    /// [`VerseWriter::append_fenced`].
    AppendFenced {
        verse: Box<Verse>,
        level: Durability,
    },
    /// [`VerseWriter::archive`].
    Archive {
        verse: Box<Verse>,
        level: Durability,
    },
    /// [`VerseWriter::record`].
    Record {
        verse: Box<Verse>,
        level: Durability,
    },
    /// [`VerseWriter::put_blob`].
    PutBlob { bytes: Vec<u8> },
    /// [`ResourceFenceStore::install_fence`].
    InstallFence { verse: Box<Verse> },
    /// [`ResourceFenceStore::installed_fence`].
    InstalledFence { singer: SingerId, task: TaskId },
    /// [`ResourceFenceStore::task_state`].
    TaskState { task: TaskId },
    /// [`RefrainSlot::refrain`].
    Refrain { refrain: Box<Refrain> },
    /// [`RefrainSlot::latest_refrains`].
    LatestRefrains,
    /// [`ScribeRead::snapshot`].
    Snapshot { close: VerseId },
    /// [`ScribeRead::task_records`].
    TaskRecords { task: TaskId },
}

/// One reply, in request order.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "reply", content = "value", rename_all = "snake_case")]
pub enum Reply {
    /// Records and the cursor to read from next.
    Records(Vec<Record>, Cursor),
    /// One record, if known.
    Record(Option<Box<Record>>),
    /// Every record of a task.
    TaskRecords(Vec<Record>),
    /// A chain head, if any.
    Head(Option<Head>),
    /// Bytes, if known.
    Bytes(Option<Vec<u8>>),
    /// A write's receipt.
    Receipt(Receipt),
    /// A blob's hash.
    Hash(Hash),
    /// A fence ack.
    Ack(FenceAck),
    /// The installed fence, if any.
    Fence(Option<Fence>),
    /// A task's state.
    TaskState(TaskState),
    /// Latest refrains.
    Refrains(Vec<Refrain>),
    /// Nothing to return.
    Done,
    /// The backend refused or failed.
    Error(WireError),
}

/// A backend error as it crosses the wire.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "error", content = "detail", rename_all = "snake_case")]
pub enum WireError {
    /// [`StoreError::Unsupported`].
    Unsupported(Durability),
    /// [`StoreError::Io`], on the service's side.
    Io(String),
    /// [`StoreError::Route`].
    Route(String),
    /// [`FenceError::Fenced`].
    Fenced,
    /// [`FenceError::Stale`].
    Stale,
    /// [`FenceError::Closed`].
    Closed,
    /// [`FenceError::BadGrant`].
    BadGrant,
}

impl From<StoreError> for WireError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::Unsupported(l) => WireError::Unsupported(l),
            StoreError::Io(s) => WireError::Io(s),
            StoreError::Route(s) => WireError::Route(s),
        }
    }
}

impl From<FenceError> for WireError {
    fn from(e: FenceError) -> Self {
        match e {
            FenceError::Fenced => WireError::Fenced,
            FenceError::Stale => WireError::Stale,
            FenceError::Closed => WireError::Closed,
            FenceError::BadGrant => WireError::BadGrant,
            FenceError::Store(s) => s.into(),
        }
    }
}

fn store_error(w: WireError) -> StoreError {
    match w {
        WireError::Unsupported(l) => StoreError::Unsupported(l),
        WireError::Io(s) => StoreError::Io(format!("store service: {s}")),
        WireError::Route(s) => StoreError::Route(s),
        other => StoreError::Io(format!("store service: unexpected {other:?}")),
    }
}

fn fence_error(w: WireError) -> FenceError {
    match w {
        WireError::Fenced => FenceError::Fenced,
        WireError::Stale => FenceError::Stale,
        WireError::Closed => FenceError::Closed,
        WireError::BadGrant => FenceError::BadGrant,
        other => FenceError::Store(store_error(other)),
    }
}

// ---- the service ------------------------------------------------------------------------------

/// Answer one request from the backend.
pub async fn handle(store: &dyn SongStore, req: Request) -> Reply {
    let r = |e: StoreError| Reply::Error(e.into());
    match req {
        Request::ReadSince { cursor } => store
            .read_since(cursor)
            .await
            .map_or_else(r, |(v, c)| Reply::Records(v, c)),
        Request::Get { id } => VerseReader::get(store, &id)
            .await
            .map_or_else(r, |x| Reply::Record(x.map(Box::new))),
        Request::Head { singer, task } => {
            store.head(&singer, &task).await.map_or_else(r, Reply::Head)
        }
        Request::GetBlob { hash } => store.get_blob(&hash).await.map_or_else(r, Reply::Bytes),
        Request::Append { verse, level } => store
            .append(&verse, level)
            .await
            .map_or_else(r, Reply::Receipt),
        Request::AppendFenced { verse, level } => store
            .append_fenced(&verse, level)
            .await
            .map_or_else(|e| Reply::Error(e.into()), Reply::Receipt),
        Request::Archive { verse, level } => store
            .archive(&verse, level)
            .await
            .map_or_else(r, Reply::Receipt),
        Request::Record { verse, level } => store
            .record(&verse, level)
            .await
            .map_or_else(r, Reply::Receipt),
        Request::PutBlob { bytes } => store.put_blob(&bytes).await.map_or_else(r, Reply::Hash),
        Request::InstallFence { verse } => {
            store.install_fence(&verse).await.map_or_else(r, Reply::Ack)
        }
        Request::InstalledFence { singer, task } => store
            .installed_fence(&singer, &task)
            .await
            .map_or_else(r, Reply::Fence),
        Request::TaskState { task } => ResourceFenceStore::task_state(store, &task)
            .await
            .map_or_else(r, Reply::TaskState),
        Request::Refrain { refrain } => store
            .refrain(&refrain)
            .await
            .map_or_else(r, |()| Reply::Done),
        Request::LatestRefrains => store
            .latest_refrains()
            .await
            .map_or_else(r, Reply::Refrains),
        Request::Snapshot { close } => store.snapshot(&close).await.map_or_else(r, Reply::Bytes),
        Request::TaskRecords { task } => store
            .task_records(&task)
            .await
            .map_or_else(r, Reply::TaskRecords),
    }
}

/// Serve one connection until the client hangs up. A line that is not a request is answered
/// with an `io` error and the connection kept.
///
/// # Errors
/// Transport failures.
pub async fn serve_connection<S>(store: Arc<dyn SongStore>, stream: S) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (read, mut write) = tokio::io::split(stream);
    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        let reply = match serde_json::from_str::<Request>(&line) {
            Ok(req) => handle(store.as_ref(), req).await,
            Err(e) => Reply::Error(WireError::Io(format!("bad request: {e}"))),
        };
        let mut out = serde_json::to_vec(&reply).map_err(std::io::Error::other)?;
        out.push(b'\n');
        write.write_all(&out).await?;
        write.flush().await?;
    }
    Ok(())
}

/// Accept on a Unix socket for ever, one task per connection.
///
/// # Errors
/// Accept failures.
pub async fn serve_unix(store: Arc<dyn SongStore>, listener: UnixListener) -> std::io::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let store = Arc::clone(&store);
        tokio::spawn(async move { serve_connection(store, stream).await });
    }
}

/// Accept on TCP for ever, one task per connection. Bind it to the chorus's private network
/// only: the protocol has no authentication of its own (the verses carry their signatures).
///
/// # Errors
/// Accept failures.
pub async fn serve_tcp(store: Arc<dyn SongStore>, listener: TcpListener) -> std::io::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let store = Arc::clone(&store);
        tokio::spawn(async move { serve_connection(store, stream).await });
    }
}

/// Bind a Unix socket at `path`, replacing a socket file left by an earlier run. The backend's
/// own lock is what guarantees a single owner; the socket file carries no state.
///
/// # Errors
/// Bind failures, or a non-socket file at `path`.
pub fn bind_unix(path: &Path) -> std::io::Result<UnixListener> {
    use std::os::unix::fs::FileTypeExt as _;
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_socket() => std::fs::remove_file(path)?,
        Ok(_) => {
            return Err(std::io::Error::other(format!(
                "{} exists and is not a socket",
                path.display()
            )));
        }
        Err(_) => {}
    }
    UnixListener::bind(path)
}

// ---- the client -------------------------------------------------------------------------------

type Reader = tokio::io::Lines<BufReader<Box<dyn AsyncRead + Send + Unpin>>>;
type Writer = Box<dyn AsyncWrite + Send + Unpin>;

/// A [`SongStore`] over the service. One connection; calls are serialized on it.
pub struct StoreClient {
    conn: Mutex<(Reader, Writer)>,
}

fn io(e: impl std::fmt::Display) -> StoreError {
    StoreError::Io(format!("store service: {e}"))
}

impl StoreClient {
    /// Connect over a Unix socket.
    ///
    /// # Errors
    /// `StoreError::Io` if the service is not there.
    pub async fn connect_unix(path: &Path) -> Result<Self, StoreError> {
        let stream = tokio::net::UnixStream::connect(path).await.map_err(io)?;
        let (r, w) = stream.into_split();
        Ok(Self::from_halves(Box::new(r), Box::new(w)))
    }

    /// Connect over TCP.
    ///
    /// # Errors
    /// `StoreError::Io` if the service is not there.
    pub async fn connect_tcp(addr: &str) -> Result<Self, StoreError> {
        let stream = tokio::net::TcpStream::connect(addr).await.map_err(io)?;
        let (r, w) = stream.into_split();
        Ok(Self::from_halves(Box::new(r), Box::new(w)))
    }

    fn from_halves(r: Box<dyn AsyncRead + Send + Unpin>, w: Writer) -> Self {
        Self {
            conn: Mutex::new((BufReader::new(r).lines(), w)),
        }
    }

    async fn call(&self, req: &Request) -> Result<Reply, StoreError> {
        let mut out = serde_json::to_vec(req).map_err(io)?;
        out.push(b'\n');
        let mut conn = self.conn.lock().await;
        conn.1.write_all(&out).await.map_err(io)?;
        conn.1.flush().await.map_err(io)?;
        let line = conn
            .0
            .next_line()
            .await
            .map_err(io)?
            .ok_or_else(|| io("connection closed"))?;
        serde_json::from_str(&line).map_err(io)
    }

    async fn expect<T>(
        &self,
        req: Request,
        pick: impl FnOnce(Reply) -> Option<T>,
    ) -> Result<T, StoreError> {
        match self.call(&req).await? {
            Reply::Error(w) => Err(store_error(w)),
            reply => pick(reply).ok_or_else(|| io("reply does not match the call")),
        }
    }
}

#[async_trait]
impl VerseReader for StoreClient {
    async fn read_since(&self, cursor: Cursor) -> Result<(Vec<Record>, Cursor), StoreError> {
        self.expect(Request::ReadSince { cursor }, |r| match r {
            Reply::Records(v, c) => Some((v, c)),
            _ => None,
        })
        .await
    }
    async fn get(&self, id: &VerseId) -> Result<Option<Record>, StoreError> {
        self.expect(Request::Get { id: *id }, |r| match r {
            Reply::Record(x) => Some(x.map(|b| *b)),
            _ => None,
        })
        .await
    }
    async fn head(&self, singer: &SingerId, task: &TaskId) -> Result<Option<Head>, StoreError> {
        let req = Request::Head {
            singer: singer.clone(),
            task: task.clone(),
        };
        self.expect(req, |r| match r {
            Reply::Head(h) => Some(h),
            _ => None,
        })
        .await
    }
    async fn get_blob(&self, h: &Hash) -> Result<Option<Vec<u8>>, StoreError> {
        self.expect(Request::GetBlob { hash: *h }, bytes).await
    }
}

fn receipt(r: Reply) -> Option<Receipt> {
    match r {
        Reply::Receipt(x) => Some(x),
        _ => None,
    }
}

#[allow(
    clippy::option_option,
    reason = "outer: the reply matched the call; inner: the blob exists"
)]
fn bytes(r: Reply) -> Option<Option<Vec<u8>>> {
    match r {
        Reply::Bytes(b) => Some(b),
        _ => None,
    }
}

#[async_trait]
impl VerseWriter for StoreClient {
    async fn append(&self, v: &Verse, level: Durability) -> Result<Receipt, StoreError> {
        self.expect(
            Request::Append {
                verse: Box::new(v.clone()),
                level,
            },
            receipt,
        )
        .await
    }
    async fn append_fenced(&self, v: &Verse, level: Durability) -> Result<Receipt, FenceError> {
        match self
            .call(&Request::AppendFenced {
                verse: Box::new(v.clone()),
                level,
            })
            .await?
        {
            Reply::Receipt(x) => Ok(x),
            Reply::Error(w) => Err(fence_error(w)),
            _ => Err(io("reply does not match the call").into()),
        }
    }
    async fn archive(&self, v: &Verse, level: Durability) -> Result<Receipt, StoreError> {
        self.expect(
            Request::Archive {
                verse: Box::new(v.clone()),
                level,
            },
            receipt,
        )
        .await
    }
    async fn record(&self, v: &Verse, level: Durability) -> Result<Receipt, StoreError> {
        self.expect(
            Request::Record {
                verse: Box::new(v.clone()),
                level,
            },
            receipt,
        )
        .await
    }
    async fn put_blob(&self, b: &[u8]) -> Result<Hash, StoreError> {
        self.expect(Request::PutBlob { bytes: b.to_vec() }, |r| match r {
            Reply::Hash(h) => Some(h),
            _ => None,
        })
        .await
    }
}

#[async_trait]
impl ResourceFenceStore for StoreClient {
    async fn install_fence(&self, grant: &Verse) -> Result<FenceAck, StoreError> {
        self.expect(
            Request::InstallFence {
                verse: Box::new(grant.clone()),
            },
            |r| match r {
                Reply::Ack(a) => Some(a),
                _ => None,
            },
        )
        .await
    }
    async fn installed_fence(
        &self,
        singer: &SingerId,
        task: &TaskId,
    ) -> Result<Option<Fence>, StoreError> {
        let req = Request::InstalledFence {
            singer: singer.clone(),
            task: task.clone(),
        };
        self.expect(req, |r| match r {
            Reply::Fence(f) => Some(f),
            _ => None,
        })
        .await
    }
    async fn task_state(&self, task: &TaskId) -> Result<TaskState, StoreError> {
        self.expect(Request::TaskState { task: task.clone() }, |r| match r {
            Reply::TaskState(s) => Some(s),
            _ => None,
        })
        .await
    }
}

#[async_trait]
impl RefrainSlot for StoreClient {
    async fn refrain(&self, r: &Refrain) -> Result<(), StoreError> {
        self.expect(
            Request::Refrain {
                refrain: Box::new(r.clone()),
            },
            |r| match r {
                Reply::Done => Some(()),
                _ => None,
            },
        )
        .await
    }
    async fn latest_refrains(&self) -> Result<Vec<Refrain>, StoreError> {
        self.expect(Request::LatestRefrains, |r| match r {
            Reply::Refrains(v) => Some(v),
            _ => None,
        })
        .await
    }
}

#[async_trait]
impl ScribeRead for StoreClient {
    async fn snapshot(&self, close: &VerseId) -> Result<Option<Vec<u8>>, StoreError> {
        self.expect(Request::Snapshot { close: *close }, bytes)
            .await
    }
    async fn blob(&self, h: &Hash) -> Result<Option<Vec<u8>>, StoreError> {
        self.get_blob(h).await
    }
    async fn task_records(&self, task: &TaskId) -> Result<Vec<Record>, StoreError> {
        self.expect(Request::TaskRecords { task: task.clone() }, |r| match r {
            Reply::TaskRecords(v) => Some(v),
            _ => None,
        })
        .await
    }
}
