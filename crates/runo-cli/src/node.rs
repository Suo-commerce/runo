//! Tier 3 at run time: `runo node` runs one voice; `runo sing` asks it to declare a state.

use std::collections::BTreeMap;
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
    /// Profile file; with a manifest, the singer's entry there is used when this is not given.
    pub profile: Option<&'a Path>,
    /// `chorus.toml`, so each peer is judged by its own profile.
    pub manifest: Option<&'a Path>,
    /// Chorus directory with the signed roster.
    pub chorus: &'a Path,
    /// The store: a Unix socket path, or `tcp:host:port`.
    pub store: &'a str,
    /// The owner's pinned key, hex.
    pub owner_key: Option<&'a str>,
    /// The control socket.
    pub control: PathBuf,
}

/// This voice's profile, and every voice's from the manifest if one was given.
fn profiles(
    a: &NodeArgs<'_>,
) -> Result<
    (
        runo_profile::Profile,
        BTreeMap<runo_core::SingerId, runo_profile::Profile>,
    ),
    String,
> {
    let mut peers = BTreeMap::new();
    let mut mine = None;
    if let Some(manifest) = a.manifest {
        let (m, loaded) = crate::chorus::load(manifest)?;
        for v in &m.voices {
            if v.singer == a.singer {
                mine = Some(loaded[&v.profile].clone());
            }
            peers.insert(
                runo_core::SingerId(v.singer.clone()),
                loaded[&v.profile].clone(),
            );
        }
    }
    if let Some(path) = a.profile {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        mine = Some(load_profile(&text).map_err(|e| format!("{}: {e}", path.display()))?);
    }
    let mine = mine.ok_or_else(|| {
        format!(
            "no profile for {}: pass --profile, or --manifest that lists it",
            a.singer
        )
    })?;
    Ok((mine, peers))
}

/// The store, retried for half a minute: on a fresh `docker compose up` the nodes can start
/// before the store listens.
async fn connect_patiently(store: &str) -> Result<StoreClient, String> {
    let mut tries = 0;
    loop {
        match connect(store).await {
            Ok(c) => return Ok(c),
            Err(e) if tries >= 15 => return Err(e),
            Err(_) => {
                tries += 1;
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}

/// `runo node`.
pub fn node(a: &NodeArgs<'_>) -> Result<(), String> {
    let (profile, peers) = profiles(a)?;
    let roster = roster(a.chorus, a.owner_key)?;
    let key = KeyHome::from_env()
        .load(a.singer)
        .map_err(|e| e.to_string())?;
    let timers = Timers {
        refrain: millis(profile.refrain_interval.as_deref().unwrap_or("15m"))?,
        probe: millis(&profile.probe_interval)?,
    };
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(async move {
        let store = connect_patiently(a.store).await?;
        let task = format!("boot-{}", wall().0);
        let mut voice = Voice::new(Arc::new(store), key, a.singer, &task, profile, roster)
            .and_then(|v| v.with_peers(&peers))
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

/// The signed roster v1 from the chorus directory, refused unless signed by the pinned owner.
fn roster(chorus: &Path, owner_key: Option<&str>) -> Result<runo_core::RosterDoc, String> {
    let pin = owner_key.map(unhex).transpose()?;
    let doc = ChorusDir::at(chorus)
        .read_roster(1, pin.as_ref())
        .map_err(|e| e.to_string())?;
    if pin.is_none() {
        eprintln!(
            "runo: the owner key is not pinned; pass --owner-key to refuse a substituted roster"
        );
    }
    Ok(doc)
}

async fn connect(store: &str) -> Result<StoreClient, String> {
    match store.strip_prefix("tcp:") {
        Some(addr) => StoreClient::connect_tcp(addr).await,
        None => StoreClient::connect_unix(Path::new(store)).await,
    }
    .map_err(|e| format!("store {store}: {e}"))
}

/// `runo view`: the fleet, one line per machine, problems first.
pub fn view(
    manifest: &Path,
    chorus: &Path,
    store: &str,
    owner_key: Option<&str>,
    every: Option<u64>,
) -> Result<(), String> {
    use std::io::IsTerminal as _;

    use runo_node::fleet::{Fleet, render};
    use runo_node::listener::Listener;

    let (m, profiles) = crate::chorus::load(manifest)?;
    let doc = roster(chorus, owner_key)?;
    let by_singer = m
        .voices
        .iter()
        .map(|v| {
            (
                runo_core::SingerId(v.singer.clone()),
                profiles[&v.profile].clone(),
            )
        })
        .collect();
    let mut fleet = Fleet::new(Listener::new(doc.clone()), by_singer).map_err(|e| e.to_string())?;
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(async {
        let client = connect(store).await?;
        loop {
            fleet.update(&client).await.map_err(|e| e.to_string())?;
            let now = crate::wall_now();
            let text = render(&doc.run, &fleet.lines(now), fleet.incidents(), now);
            let Some(secs) = every else {
                print!("{text}");
                return Ok(());
            };
            if std::io::stdout().is_terminal() {
                print!("\x1b[2J\x1b[H");
            }
            println!("{text}");
            tokio::select! {
                () = tokio::time::sleep(Duration::from_secs(secs)) => {}
                _ = tokio::signal::ctrl_c() => return Ok(()),
            }
        }
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
