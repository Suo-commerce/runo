//! Deterministic simulation harness: N seeds per scenario, S1–S4 checked after every step,
//! bounded liveness at the end, exact replay from a seed (technical spec §12).

use clap::Parser;

/// Run one scenario under one seed, or all scenarios under many.
#[derive(Parser, Debug)]
#[command(name = "runo-sim")]
struct Args {
    /// Scenario file under `scenarios/`.
    #[arg(long)]
    scenario: Option<String>,
    /// Seed to replay exactly.
    #[arg(long)]
    seed: Option<u64>,
    /// Seeds per scenario when sweeping.
    #[arg(long, default_value_t = 1000)]
    seeds: u64,
}

fn main() {
    let args = Args::parse();
    println!("runo-sim: {args:?} (milestone 5)");
}
