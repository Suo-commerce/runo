//! `runo node`'s loop: refrain and probe on timers, and sing what the host declares through a
//! control socket (`runo sing sleep` from a suspend hook). One task owns the [`Voice`], so a
//! declared state and a refrain can never race on the chain.
//!
//! **Control protocol.** One JSON line per connection, `{"event": "sleep"}` or `{"event":
//! "waiting", "blocked_on": "vpn"}`, answered by one line, `{"sung": "<verse id>"}` or
//! `{"refused": "<why>"}`. The socket is the host's own: the node creates it mode 0600.
//!
//! **Calls.** After every probe, crossings go to the [`Ladder`]; what it owes a person goes to the
//! [`HumanChannel`]; a newly suspected peer gets a signed `Suspect` verse. The log carries the
//! node's own events (what it sang, incidents, failures), never one line per tick.

use std::os::unix::net::UnixStream as UnixStreamStd;
use std::path::Path;
use std::time::Duration;

use runo_core::Timestamp;
use runo_core::hash::display;
use runo_core::time::Clock;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::channel::human::HumanChannel;
use crate::ladder::Ladder;
use crate::voice::{Heard, Voice, VoiceError};
use runo_store::{RefrainSlot, VerseReader, VerseWriter};

/// A request on the control socket.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SingRequest {
    /// The state to declare.
    pub event: String,
    /// What it waits on, for `waiting`.
    #[serde(default)]
    pub blocked_on: Option<String>,
}

/// The node's answer.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum SingReply {
    /// The verse was sung; its id.
    Sung(String),
    /// It was not; why.
    Refused(String),
}

/// How often the loop refrains and probes. `Duration` only here, at the timer boundary.
#[derive(Clone, Copy, Debug)]
pub struct Timers {
    /// Between refrains.
    pub refrain: Duration,
    /// Between probes.
    pub probe: Duration,
}

/// Bind the control socket, readable and writable by this user only.
///
/// # Errors
/// The path is in use by a live node, or cannot be created.
pub fn bind_control(path: &Path) -> std::io::Result<UnixListener> {
    use std::os::unix::fs::PermissionsExt as _;
    if UnixStreamStd::connect(path).is_ok() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!("{} is held by a running node", path.display()),
        ));
    }
    let _ = std::fs::remove_file(path);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let l = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(l)
}

/// Ask the running node to sing `req`.
///
/// # Errors
/// No node on `path`, or it closed without answering.
pub async fn ask(path: &Path, req: &SingRequest) -> std::io::Result<SingReply> {
    let stream = UnixStream::connect(path).await.map_err(|e| {
        std::io::Error::new(e.kind(), format!("no node on {}: {e}", path.display()))
    })?;
    let (r, mut w) = stream.into_split();
    let mut line = serde_json::to_string(req).map_err(std::io::Error::other)?;
    line.push('\n');
    w.write_all(line.as_bytes()).await?;
    let mut answer = String::new();
    BufReader::new(r).read_line(&mut answer).await?;
    serde_json::from_str(&answer).map_err(std::io::Error::other)
}

/// What the loop talks to besides the voice: time, the log, the ladder and the person.
pub struct Env<'a> {
    /// Monotonic time for the clocks.
    pub clock: &'a dyn Clock,
    /// Wall time for `sung_at` and the ledger.
    pub wall: &'a (dyn Fn() -> Timestamp + Sync),
    /// The node's own events, one line each.
    pub log: &'a mut (dyn FnMut(String) + Send),
    /// Episodes and the messages they owe.
    pub ladder: &'a mut Ladder,
    /// Where a person reads them.
    pub channel: &'a dyn HumanChannel,
}

/// Run until `stop` resolves: start, then refrain, probe and serve the control socket.
///
/// # Errors
/// The start failed; later store errors are logged and retried on the next timer.
pub async fn run<S>(
    voice: &mut Voice<S>,
    env: &mut Env<'_>,
    control: UnixListener,
    timers: Timers,
    stop: impl std::future::Future<Output = ()>,
) -> Result<(), VoiceError>
where
    S: VerseReader + VerseWriter + RefrainSlot + ?Sized,
{
    let heard = voice.start(env.clock.now(), &(env.wall)()).await?;
    let line = format!(
        "{} sang {} on {}",
        (env.wall)().0,
        voice.state().unwrap_or("?"),
        voice.task().0
    );
    (env.log)(line);
    take(voice, env, &heard).await;
    let mut refrain = tokio::time::interval(timers.refrain);
    let mut probe = tokio::time::interval(timers.probe);
    tokio::pin!(stop);
    loop {
        tokio::select! {
            () = &mut stop => return Ok(()),
            _ = refrain.tick() => {
                if let Err(e) = voice.refrain(&(env.wall)()).await {
                    (env.log)(format!("{} refrain failed, retrying next interval: {e}", (env.wall)().0));
                }
            }
            _ = probe.tick() => match voice.probe(env.clock.now()).await {
                Ok(h) => take(voice, env, &h).await,
                Err(e) => (env.log)(format!("{} probe failed, retrying next interval: {e}", (env.wall)().0)),
            },
            conn = control.accept() => {
                if let Ok((stream, _)) = conn {
                    serve_one(voice, env, stream).await;
                }
            }
        }
    }
}

/// What a probe heard: incidents to the log, crossings to the ladder, `Suspect` for a newly
/// suspected peer, then every owed message to the person.
pub async fn take<S>(voice: &mut Voice<S>, env: &mut Env<'_>, heard: &Heard)
where
    S: VerseReader + VerseWriter + RefrainSlot + ?Sized,
{
    report(heard, env.log);
    let now = env.clock.now();
    let wall = (env.wall)();
    for e in env.ladder.record(voice, heard, now, &wall) {
        match voice.suspect(now, &wall, &e).await {
            Ok(_) => (env.log)(format!(
                "{} sang suspect about {} ({:?})",
                wall.0, e.peer.0, e.clock
            )),
            Err(err) => (env.log)(format!(
                "{} could not sing suspect about {}: {err}",
                wall.0, e.peer.0
            )),
        }
    }
    for o in env.ladder.owed() {
        match env.channel.deliver(&o.notice, &o.text).await {
            Ok(()) => {
                if let Err(err) = env.ladder.delivered(&o) {
                    (env.log)(format!("{} episode ledger not saved: {err}", wall.0));
                }
            }
            Err(err) => (env.log)(format!("{} message not delivered, retrying: {err}", wall.0)),
        }
    }
}

async fn serve_one<S>(voice: &mut Voice<S>, env: &mut Env<'_>, stream: UnixStream)
where
    S: VerseReader + VerseWriter + RefrainSlot + ?Sized,
{
    let (r, mut w) = stream.into_split();
    let mut line = String::new();
    let mut r = BufReader::new(r);
    let read = tokio::time::timeout(Duration::from_secs(5), r.read_line(&mut line)).await;
    let reply = match read {
        Ok(Ok(_)) => match serde_json::from_str::<SingRequest>(&line) {
            Ok(req) => match voice
                .sing(env.clock.now(), &(env.wall)(), &req.event, req.blocked_on)
                .await
            {
                Ok(id) => {
                    (env.log)(format!("{} sang {}", (env.wall)().0, req.event));
                    SingReply::Sung(display(&id.0))
                }
                Err(e) => SingReply::Refused(e.to_string()),
            },
            Err(e) => SingReply::Refused(format!("not a sing request: {e}")),
        },
        _ => SingReply::Refused("no request within five seconds".into()),
    };
    if let Ok(mut out) = serde_json::to_string(&reply) {
        out.push('\n');
        let _ = w.write_all(out.as_bytes()).await;
    }
}

/// Incidents, one line each: a record that failed ingress is never believed, and always said.
pub fn report(heard: &Heard, log: &mut (dyn FnMut(String) + Send)) {
    for (singer, why) in &heard.incidents {
        log(format!(
            "incident: a record claiming to be {} was refused ({why:?}); it was not believed",
            singer.0
        ));
    }
}
