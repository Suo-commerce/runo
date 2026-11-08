//! Deterministic simulation harness: N seeds per scenario, S1–S4 checked after every step,
//! bounded liveness at the end, exact replay from a seed (technical spec §12).

use clap::Parser;
use runo_sim::scenarios::{ALL, run};

/// Run one scenario under one seed, or all scenarios under many.
#[derive(Parser, Debug)]
#[command(name = "runo-sim")]
struct Args {
    /// Scenario name (a file under `scenarios/`, without `.toml`); every scenario if omitted.
    #[arg(long)]
    scenario: Option<String>,
    /// Replay exactly this seed, printing its trace.
    #[arg(long)]
    seed: Option<u64>,
    /// Seeds per scenario when sweeping.
    #[arg(long, default_value_t = 1000)]
    seeds: u64,
}

fn main() {
    let args = Args::parse();
    let names: Vec<String> = args.scenario.clone().map_or_else(
        || ALL.iter().map(|s| (*s).to_owned()).collect(),
        |s| vec![s],
    );
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a single-threaded runtime");
    let mut failed = false;
    for name in &names {
        if let Some(seed) = args.seed {
            let (r, trace) = rt.block_on(run(name, seed));
            for line in &trace {
                println!("  {line}");
            }
            match r {
                Ok(()) => println!("{name} seed {seed}: holds"),
                Err(why) => {
                    println!("{name} seed {seed}: FAILED: {why}");
                    failed = true;
                }
            }
            continue;
        }
        let start = std::time::Instant::now();
        let mut first_failure = None;
        for seed in 0..args.seeds {
            let (r, _) = rt.block_on(run(name, seed));
            if let Err(why) = r {
                first_failure = Some((seed, why));
                break;
            }
        }
        match first_failure {
            None => println!(
                "{name:<22} {} seeds hold ({:.1}s)",
                args.seeds,
                start.elapsed().as_secs_f64()
            ),
            Some((seed, why)) => {
                failed = true;
                println!("{name:<22} FAILED at seed {seed}: {why}");
                println!("  replay: cargo run -p runo-sim -- --scenario {name} --seed {seed}");
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
}
