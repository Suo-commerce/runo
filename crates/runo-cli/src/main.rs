//! The `runo` binary (technical spec §11, §15). Three tiers: diagnose, watch, chorus.

use clap::{Parser, Subcommand};

/// Runo: the Dwarven Song.
#[derive(Parser, Debug)]
#[command(name = "runo", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Tier 1: the replay report on an existing pipeline. Read-only.
    Diagnose {
        /// Paperclip database URL or jsonl export directory.
        #[arg(long)]
        paperclip: String,
        /// Forge base URL, for review loops.
        #[arg(long)]
        forge: Option<String>,
    },
    /// Tier 2: the triage view and call ladder, as a chorus of one.
    Watch {
        /// Paperclip database URL.
        #[arg(long)]
        paperclip: String,
        /// Where to deliver, e.g. `telegram:<chat>`.
        #[arg(long)]
        notify: Option<String>,
    },
    /// What runo can reach and what it would do; every default printed.
    Doctor,
    /// Tier 3: write the genesis roster from a chorus manifest.
    Init {
        /// Path to chorus.toml.
        manifest: String,
    },
    /// Tier 3: generate this voice's key and submit its roster entry.
    Enroll {
        /// Singer id.
        #[arg(long)]
        singer: String,
    },
    /// Tier 3: run a voice.
    Node {
        /// Profile path.
        #[arg(long)]
        profile: String,
    },
    /// Sing one verse through the local node (or record-only to the store).
    Sing {
        /// Verse kind and arguments; see `runo sing --help`.
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Latest verse and refrain per voice, four clocks, suspicion.
    Listen {
        /// Only overdue crossings.
        #[arg(long)]
        overdue: bool,
    },
    /// The owner's view.
    View,
    /// The chorus's store.
    Store {
        #[command(subcommand)]
        cmd: StoreCmd,
    },
}

#[derive(Subcommand, Debug)]
enum StoreCmd {
    /// Own the chorus's jsonl store and serve it to nodes and commands; one per chorus.
    Serve {
        /// The chorus directory (created if missing).
        #[arg(long)]
        dir: std::path::PathBuf,
        /// The Unix socket to serve on [default: <dir>/store.sock].
        #[arg(long)]
        socket: Option<std::path::PathBuf>,
        /// Also serve TCP on this address; bind it to the chorus's private network only.
        #[arg(long)]
        tcp: Option<String>,
        /// `host` (fsync before acknowledging) or `process`.
        #[arg(long, default_value = "host")]
        durability: String,
    },
}

/// `runo store serve`: open the backend, then serve until stopped.
fn store_serve(
    dir: &std::path::Path,
    socket: Option<std::path::PathBuf>,
    tcp: Option<String>,
    durability: &str,
) -> Result<(), String> {
    use runo_core::fence::Durability;
    use runo_store::service::{bind_unix, serve_tcp, serve_unix};
    use std::sync::Arc;

    let level = match durability {
        "host" => Durability::Host,
        "process" => Durability::Process,
        other => return Err(format!("durability must be host or process, not {other}")),
    };
    let store =
        runo_store::jsonl::JsonlStore::open(dir, runo_core::ResourceId("song".into()), level)
            .map_err(|e| e.to_string())?;
    let store: Arc<dyn runo_store::SongStore> = Arc::new(store);
    let socket = socket.unwrap_or_else(|| dir.join("store.sock"));
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(async move {
        let unix = bind_unix(&socket).map_err(|e| e.to_string())?;
        println!("runo store: {} on {}", dir.display(), socket.display());
        let tcp_task = match tcp {
            Some(addr) => {
                let listener = tokio::net::TcpListener::bind(&addr)
                    .await
                    .map_err(|e| e.to_string())?;
                println!("runo store: also on tcp {addr}");
                Some(tokio::spawn(serve_tcp(Arc::clone(&store), listener)))
            }
            None => None,
        };
        tokio::select! {
            r = serve_unix(store, unix) => r.map_err(|e| e.to_string()),
            _ = tokio::signal::ctrl_c() => {
                if let Some(t) = tcp_task { t.abort(); }
                let _ = std::fs::remove_file(&socket);
                Ok(())
            }
        }
    })
}

fn main() {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Diagnose { .. } => {
            println!("runo diagnose: until milestone 3, run runo-replay/replay.py");
        }
        Cmd::Doctor => {
            println!("runo doctor: nothing configured yet; every default will be printed here");
        }
        Cmd::Store {
            cmd:
                StoreCmd::Serve {
                    dir,
                    socket,
                    tcp,
                    durability,
                },
        } => {
            if let Err(e) = store_serve(&dir, socket, tcp, &durability) {
                eprintln!("runo store serve: {e}");
                std::process::exit(3);
            }
        }
        other => println!(
            "{other:?}: not yet implemented; see the technical specification §14 for its milestone"
        ),
    }
}
