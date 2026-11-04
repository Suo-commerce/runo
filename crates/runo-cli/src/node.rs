//! Tier 3 at run time: `runo node` runs one voice; `runo sing` asks it to declare a state.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use runo_core::Timestamp;
use runo_core::time::MonotonicClock;
use runo_node::identity::{ChorusDir, KeyHome};
use runo_node::run::{SingReply, SingRequest, Timers, ask, bind_control, run};
use runo_node::voice::{Voice, interval};
use runo_profile::load_profile;
use runo_store::service::StoreClient;

/// Where a singer's control socket lives unless given: `$XDG_RUNTIME_DIR/runo/<singer>.sock`,
/// else `~/.local/state/runo/<singer>.sock`.
pub fn control_path(singer: &str, given: Option<PathBuf>) -> PathBuf {
    given.unwrap_or_else(|| {
        let base = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(
            || PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"),
            PathBuf::from,
        );
        base.join("runo").join(format!("{singer}.sock"))
    })
}

fn wall() -> Timestamp {
    Timestamp(runo_adapter_paperclip::time::format(crate::wall_now()))
}

fn unhex(s: &str) -> Result<[u8; 32], String> {
    let bytes: Result<Vec<u8>, _> = (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2).unwrap_or("x"), 16))
        .collect();
    bytes
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| "--owner-key is the 64 hex characters `runo init --sign` printed".into())
}

fn millis(s: &str) -> Result<Duration, String> {
    interval(s)
        .map(|m| Duration::from_millis(m.0))
        .map_err(|e| e.to_string())
}

/// The node's settings from the command line.
pub struct NodeArgs<'a> {
    /// Singer id.
    pub singer: &'a str,
    /// Profile file.
    pub profile: &'a Path,
    /// Chorus directory with the signed roster.
    pub chorus: &'a Path,
    /// The store: a Unix socket path, or `tcp:host:port`.
    pub store: &'a str,
    /// The owner's pinned key, hex.
    pub owner_key: Option<&'a str>,
    /// The control socket.
    pub control: PathBuf,
}

/// `runo node`.
pub fn node(a: &NodeArgs<'_>) -> Result<(), String> {
    let text =
        std::fs::read_to_string(a.profile).map_err(|e| format!("{}: {e}", a.profile.display()))?;
    let profile = load_profile(&text).map_err(|e| format!("{}: {e}", a.profile.display()))?;
    let pin = a.owner_key.map(unhex).transpose()?;
    let roster = ChorusDir::at(a.chorus)
        .read_roster(1, pin.as_ref())
        .map_err(|e| e.to_string())?;
    if pin.is_none() {
        println!(
            "runo node: the owner key is not pinned; pass --owner-key to refuse a substituted roster"
        );
    }
    let key = KeyHome::from_env()
        .load(a.singer)
        .map_err(|e| e.to_string())?;
    let timers = Timers {
        refrain: millis(profile.refrain_interval.as_deref().unwrap_or("15m"))?,
        probe: millis(&profile.probe_interval)?,
    };
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(async move {
        let store = match a.store.strip_prefix("tcp:") {
            Some(addr) => StoreClient::connect_tcp(addr).await,
            None => StoreClient::connect_unix(Path::new(a.store)).await,
        }
        .map_err(|e| format!("store {}: {e}", a.store))?;
        let task = format!("boot-{}", wall().0);
        let mut voice = Voice::new(Arc::new(store), key, a.singer, &task, profile, roster)
            .map_err(|e| e.to_string())?;
        let control = bind_control(&a.control).map_err(|e| e.to_string())?;
        println!(
            "runo node: {} on {}, control {}",
            a.singer,
            a.store,
            a.control.display()
        );
        let clock = MonotonicClock::default();
        let mut log = |l: String| println!("{l}");
        let stop = async {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                () = async { match term.as_mut() { Some(t) => { t.recv().await; } None => std::future::pending().await } } => {}
            }
        };
        let r = run(&mut voice, &clock, &wall, control, timers, &mut log, stop).await;
        let _ = std::fs::remove_file(&a.control);
        r.map_err(|e| e.to_string())
    })
}

/// `runo sing`: ask the running node to declare `event`.
pub fn sing(control: &Path, event: &str, blocked_on: Option<String>) -> Result<(), String> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let req = SingRequest {
        event: event.to_owned(),
        blocked_on,
    };
    match rt.block_on(ask(control, &req)).map_err(|e| e.to_string())? {
        SingReply::Sung(id) => {
            println!("sang {event} ({id})");
            Ok(())
        }
        SingReply::Refused(why) => Err(why),
    }
}
