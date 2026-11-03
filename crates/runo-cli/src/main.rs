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
        other => println!(
            "{other:?}: not yet implemented; see the technical specification §14 for its milestone"
        ),
    }
}
