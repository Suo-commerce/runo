//! The `runo` binary (technical spec §11, §15). Three tiers: diagnose, watch, chorus.

mod chorus;
mod node;

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
    /// Tier 2: the triage view and call ladder, as a chorus of one. Read-only on the board.
    Watch {
        #[command(flatten)]
        board: Board,
        /// Where messages go. Only `terminal` for now; comments on the issue come behind --apply.
        #[arg(long, default_value = "terminal")]
        notify: String,
        /// Seconds between polls.
        #[arg(long, default_value_t = 60)]
        interval: u64,
        /// Poll once, print the view, and exit.
        #[arg(long)]
        once: bool,
    },
    /// What runo can reach and what it would do; every default printed. Writes nothing.
    Doctor {
        #[command(flatten)]
        board: Board,
    },
    /// Tier 3, once by the owner: check chorus.toml and make roster v1 from the enrolments.
    Init {
        /// Path to chorus.toml.
        manifest: std::path::PathBuf,
        /// The chorus directory (public files, shared by every host).
        #[arg(long)]
        chorus: std::path::PathBuf,
        /// Sign roster v1 with the owner's key from this host's key home.
        #[arg(long)]
        sign: bool,
    },
    /// Tier 3, on each host: make this voice's key and write its enrolment.
    Enroll {
        /// Singer id, as in chorus.toml.
        #[arg(long)]
        singer: String,
        /// The chorus directory.
        #[arg(long)]
        chorus: std::path::PathBuf,
        /// The credential reference to fingerprint [default: `file://keys/<singer>.key`].
        #[arg(long)]
        credential: Option<String>,
    },
    /// Tier 3: run a voice. It claims a boot session, declares the profile's first state,
    /// refrains and listens until stopped.
    Node {
        /// Singer id, as enrolled.
        #[arg(long)]
        singer: String,
        /// Profile path [default: this singer's profile in --manifest].
        #[arg(long)]
        profile: Option<std::path::PathBuf>,
        /// chorus.toml: judge each peer by its own profile (a sentinel refraining every 5
        /// minutes is missed sooner than a laptop).
        #[arg(long)]
        manifest: Option<std::path::PathBuf>,
        /// The chorus directory holding the signed roster.
        #[arg(long)]
        chorus: std::path::PathBuf,
        /// The store: its Unix socket path, or `tcp:host:port`.
        #[arg(long)]
        store: String,
        /// The owner's public key as `runo init --sign` printed it; a roster signed by any other
        /// key is refused.
        #[arg(long)]
        owner_key: Option<String>,
        /// The control socket [default: `$XDG_RUNTIME_DIR/runo/<singer>.sock`].
        #[arg(long)]
        control: Option<std::path::PathBuf>,
        /// The episode ledger, so nothing is announced twice across restarts [default:
        /// `$XDG_STATE_HOME/runo/node/<singer>.json`].
        #[arg(long)]
        state: Option<std::path::PathBuf>,
        /// Where messages go: `terminal`, `telegram`; repeat for both.
        #[arg(long, default_value = "terminal")]
        notify: Vec<String>,
        /// The Telegram chat to write to (its numeric id).
        #[arg(long)]
        telegram_chat: Option<String>,
        /// The environment variable holding the Telegram bot token (never the token itself).
        #[arg(long, default_value = "RUNO_TELEGRAM_TOKEN")]
        telegram_token_env: String,
    },
    /// Declare a state through the running node: `runo sing sleep`, `runo sing wake`.
    Sing {
        /// The state, as named in the profile.
        event: String,
        /// The singer whose node to ask.
        #[arg(long)]
        singer: String,
        /// What it waits on, with `waiting`.
        #[arg(long)]
        blocked_on: Option<String>,
        /// The node's control socket [default: `$XDG_RUNTIME_DIR/runo/<singer>.sock`].
        #[arg(long)]
        control: Option<std::path::PathBuf>,
    },
    /// Latest verse and refrain per voice, four clocks, suspicion.
    Listen {
        /// Only overdue crossings.
        #[arg(long)]
        overdue: bool,
    },
    /// The fleet: one line per machine, what it last declared and whether its silence is
    /// expected. Problems first. Read-only.
    View {
        /// Path to chorus.toml (each machine is judged by its own profile).
        #[arg(long)]
        manifest: std::path::PathBuf,
        /// The chorus directory holding the signed roster.
        #[arg(long)]
        chorus: std::path::PathBuf,
        /// The store: its Unix socket path, or `tcp:host:port`.
        #[arg(long)]
        store: String,
        /// The owner's public key as `runo init --sign` printed it.
        #[arg(long)]
        owner_key: Option<String>,
        /// Print once and exit.
        #[arg(long)]
        once: bool,
        /// Seconds between refreshes.
        #[arg(long, default_value_t = 30)]
        every: u64,
    },
    /// The chorus's store.
    Store {
        #[command(subcommand)]
        cmd: StoreCmd,
    },
}

/// Which board to read, and with what key.
#[derive(clap::Args, Debug)]
struct Board {
    /// The Paperclip board's address, such as `https://board.example`.
    #[arg(long)]
    paperclip: String,
    /// The company id on that board; `runo doctor` without it lists the companies the key sees.
    #[arg(long)]
    company: Option<String>,
    /// The environment variable holding the API key (never the key itself).
    #[arg(long, default_value = "PAPERCLIP_API_KEY")]
    token_env: String,
    /// Rows per page; the board serves at most 1000.
    #[arg(long, default_value_t = 1000)]
    page: usize,
    /// Minutes a lane may go without progress, or in progress, before it is overdue.
    #[arg(long, default_value_t = 60)]
    lane_minutes: u64,
    /// Minutes a wait (blocked, in review) may run before it is overdue.
    #[arg(long, default_value_t = 60)]
    waiting_minutes: u64,
    /// Where the episode ledger lives [default: `$XDG_STATE_HOME/runo/watch`, else
    /// `~/.local/state/runo/watch`].
    #[arg(long)]
    state: Option<std::path::PathBuf>,
}

impl Board {
    fn state_dir(&self) -> std::path::PathBuf {
        self.state.clone().unwrap_or_else(|| {
            let base = std::env::var_os("XDG_STATE_HOME").map_or_else(
                || {
                    std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                        .join(".local/state")
                },
                std::path::PathBuf::from,
            );
            base.join("runo/watch")
        })
    }

    fn client(&self) -> Result<runo_adapter_paperclip::poll::Paperclip, String> {
        let token = std::env::var(&self.token_env)
            .ok()
            .filter(|t| !t.is_empty());
        let company = self.company.as_deref().unwrap_or_default();
        runo_adapter_paperclip::poll::Paperclip::new(&self.paperclip, company, token, self.page)
            .map_err(|e| e.to_string())
    }

    fn herald(&self) -> runo_adapter_paperclip::herald::HeraldConfig {
        runo_adapter_paperclip::herald::HeraldConfig::with_minutes(
            self.lane_minutes,
            self.waiting_minutes,
        )
    }
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

fn wall_now() -> runo_core::time::Instant {
    runo_core::time::wall::now()
}

/// `runo doctor`: what can be reached, what would be done, every setting. Writes nothing.
fn doctor(board: &Board) -> Result<(), String> {
    let cfg = board.herald();
    let minutes = |m: runo_core::Millis| m.0 / 60_000;
    println!("runo doctor · read-only: nothing is written to the board\n");
    println!("board          {}", board.paperclip);
    println!(
        "company        {}",
        board.company.as_deref().unwrap_or("(not given)")
    );
    let has_key = std::env::var(&board.token_env).is_ok_and(|t| !t.is_empty());
    println!(
        "api key        ${} is {}",
        board.token_env,
        if has_key { "set" } else { "NOT set" }
    );
    println!(
        "page size      {}",
        board.page.clamp(1, runo_adapter_paperclip::poll::MAX_PAGE)
    );
    println!(
        "state          {}",
        board.state_dir().join("episodes.json").display()
    );
    println!(
        "lane promise   overdue at {}m, still overdue at {}m, needs a person at {}m, after one more {}m of grace ({})",
        minutes(cfg.promise.cadence.call),
        minutes(cfg.promise.cadence.again),
        minutes(cfg.promise.cadence.suspect),
        minutes(cfg.promise.cadence.call),
        cfg.promise.source
    );
    println!(
        "waiting        overdue at {}m, still overdue at {}m, needs a person at {}m, after one more {}m of grace",
        minutes(cfg.waiting.call),
        minutes(cfg.waiting.again),
        minutes(cfg.waiting.suspect),
        minutes(cfg.waiting.call)
    );
    println!(
        "never clears   {} Paperclip error codes it does not retry: said at once",
        cfg.never_clears.len()
    );
    println!(
        "reachability   unknown in Watch mode: the board does not say whether a worker responds"
    );
    println!("delivery       terminal; history before the first start is kept, not sent\n");

    let client = board.client()?;
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(async {
        match client.version().await {
            Ok(v) => println!(
                "reach          ok, Paperclip {}",
                v.as_deref().unwrap_or("(version not given)")
            ),
            Err(e) => return Err(format!("cannot reach the board: {e}")),
        }
        if board.company.is_none() {
            let companies = client
                .companies()
                .await
                .map_err(|e| format!("companies: {e}"))?;
            println!("\ncompanies this key can open; pass one with --company <id>:");
            for c in companies {
                println!(
                    "  {}  {}  ({})",
                    c.id,
                    c.name,
                    c.issue_prefix.as_deref().unwrap_or("no prefix")
                );
            }
            return Ok(());
        }
        let company = client
            .company()
            .await
            .map_err(|e| format!("company: {e}"))?;
        println!(
            "company name   {} (issues {}-…)",
            company.name,
            company.issue_prefix.as_deref().unwrap_or("?")
        );
        counts(&client).await
    })
}

/// The counts `runo doctor` prints after reaching the board.
async fn counts(client: &runo_adapter_paperclip::poll::Paperclip) -> Result<(), String> {
    let agents = client.agents().await.map_err(|e| format!("agents: {e}"))?;
    let issues = client.issues().await.map_err(|e| format!("issues: {e}"))?;
    let runs = client.runs().await.map_err(|e| format!("runs: {e}"))?;
    let open = client
        .open_issues()
        .await
        .map_err(|e| format!("open issues: {e}"))?;
    let count = |s: &str| open.iter().filter(|i| i.status == s).count();
    println!(
        "read           {} agents, {} recently updated issues, {} runs",
        agents.len(),
        issues.len(),
        runs.len()
    );
    println!(
        "open issues    {}: {} in progress, {} blocked, {} in review (all of them, however old)",
        open.len(),
        count("in_progress"),
        count("blocked"),
        count("in_review")
    );
    let unassigned = open
        .iter()
        .filter(|i| i.assignee_agent_id.is_none())
        .count();
    if unassigned > 0 {
        println!("               {unassigned} of them have no agent assigned and are not watched");
    }
    if open.len() >= runo_adapter_paperclip::poll::MAX_PAGE {
        println!("note           the open list filled a whole page: some open issues are not seen");
    }
    Ok(())
}

/// `runo watch`: poll, step, deliver, repeat.
fn watch(board: &Board, notify: &str, interval: u64, once: bool) -> Result<(), String> {
    use runo_adapter_paperclip::poll::PollError;
    use runo_adapter_paperclip::watch::Watch;
    use runo_node::channel::human::{HumanChannel, Terminal};

    if notify != "terminal" {
        return Err(format!(
            "--notify {notify}: only `terminal` is available so far"
        ));
    }
    if board.company.is_none() {
        return Err("--company <id> is needed; `runo doctor` without it lists the ids".into());
    }
    let client = board.client()?;
    let dir = board.state_dir();
    let mut w = Watch::open(board.herald(), &dir, &board.paperclip).map_err(|e| e.to_string())?;
    let channel = Terminal::new(std::io::stdout());
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(async {
        match client.agents().await {
            Ok(a) => w.set_agents(&a),
            Err(e) => eprintln!("runo watch: no agent names ({e}); showing ids"),
        }
        match client.company().await {
            Ok(c) => {
                if let Some(p) = &c.issue_prefix {
                    w.set_prefix(p);
                }
            }
            Err(e) => eprintln!("runo watch: no issue prefix ({e}); links omit it"),
        }
        let mut wait = interval.max(5);
        let mut last_open = None;
        loop {
            let read = async {
                Ok::<_, PollError>((
                    client.issues().await?,
                    client.open_issues().await?,
                    client.runs().await?,
                ))
            };
            match read.await {
                Ok((issues, open, runs)) => {
                    wait = interval.max(5);
                    let now = wall_now();
                    let out = w
                        .step(&issues, &open, &runs, client.page(), now)
                        .map_err(|e| e.to_string())?;
                    for o in &out {
                        channel
                            .deliver(&o.notice, &o.text)
                            .await
                            .map_err(|e| e.to_string())?;
                        w.delivered(&o.notice).map_err(|e| e.to_string())?;
                    }
                    let view = w.view(now);
                    let open = view
                        .lines()
                        .skip_while(|l| !l.starts_with("Open"))
                        .take_while(|l| !l.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n");
                    if once || last_open.as_ref() != Some(&open) || !out.is_empty() {
                        println!("\n{view}");
                        last_open = Some(open);
                    }
                }
                Err(PollError::Throttled(code)) => {
                    wait = (wait * 2).min(600);
                    eprintln!(
                        "runo watch: the board asked to back off ({code}); next poll in {wait}s"
                    );
                }
                Err(e) => eprintln!("runo watch: {e}; next poll in {wait}s"),
            }
            if once {
                return Ok(());
            }
            tokio::select! {
                () = tokio::time::sleep(std::time::Duration::from_secs(wait)) => {}
                _ = tokio::signal::ctrl_c() => return Ok(()),
            }
        }
    })
}

fn exit_on(r: Result<(), String>, cmd: &str, code: i32) {
    if let Err(e) = r {
        eprintln!("runo {cmd}: {e}");
        std::process::exit(code);
    }
}

/// Tier 3 at run time: `runo node`, `runo sing`, `runo view`. Exit code 5 on refusal.
fn chorus_run(cmd: Cmd) {
    match cmd {
        Cmd::Node {
            singer,
            profile,
            manifest,
            chorus,
            store,
            owner_key,
            control,
            state,
            notify,
            telegram_chat,
            telegram_token_env,
        } => {
            let control = node::control_path(&singer, control);
            let state = node::state_path(&singer, state);
            exit_on(
                node::node(&node::NodeArgs {
                    singer: &singer,
                    profile: profile.as_deref(),
                    manifest: manifest.as_deref(),
                    chorus: &chorus,
                    store: &store,
                    owner_key: owner_key.as_deref(),
                    control,
                    state,
                    notify: &notify,
                    telegram_chat: telegram_chat.as_deref(),
                    telegram_token_env: &telegram_token_env,
                }),
                "node",
                5,
            );
        }
        Cmd::Sing {
            event,
            singer,
            blocked_on,
            control,
        } => exit_on(
            node::sing(&node::control_path(&singer, control), &event, blocked_on),
            "sing",
            5,
        ),
        Cmd::View {
            manifest,
            chorus,
            store,
            owner_key,
            once,
            every,
        } => exit_on(
            node::view(
                &manifest,
                &chorus,
                &store,
                owner_key.as_deref(),
                (!once).then_some(every),
            ),
            "view",
            5,
        ),
        _ => unreachable!("chorus_run is called for node, sing and view only"),
    }
}

fn main() {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Diagnose { .. } => {
            println!("runo diagnose: until milestone 3, run runo-replay/replay.py");
        }
        Cmd::Doctor { board } => {
            if let Err(e) = doctor(&board) {
                eprintln!("runo doctor: {e}");
                std::process::exit(2);
            }
        }
        Cmd::Watch {
            board,
            notify,
            interval,
            once,
        } => {
            if let Err(e) = watch(&board, &notify, interval, once) {
                eprintln!("runo watch: {e}");
                std::process::exit(2);
            }
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
        Cmd::Enroll {
            singer,
            chorus,
            credential,
        } => exit_on(
            chorus::enroll(&singer, &chorus, credential.as_deref()),
            "enroll",
            4,
        ),
        Cmd::Init {
            manifest,
            chorus,
            sign,
        } => exit_on(chorus::init(&manifest, &chorus, sign), "init", 4),
        cmd @ (Cmd::Node { .. } | Cmd::Sing { .. } | Cmd::View { .. }) => chorus_run(cmd),
        other => println!(
            "{other:?}: not yet implemented; see the technical specification §14 for its milestone"
        ),
    }
}
