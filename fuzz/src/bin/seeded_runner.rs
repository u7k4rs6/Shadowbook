//! The seeded volume runner: long sequences, no shrinking, a `StdRng`
//! seeded once so any failure is reproducible by rerunning with the same
//! seed. This is the actual instrument the exit criterion (10^8
//! operations, zero divergences, zero invariant violations) is measured
//! against -- not the proptest runner (small cases, for shrinking) and
//! not cargo-fuzz if present (a coverage-guided smoke test, not
//! comparable volume, since libfuzzer grows inputs from empty and most
//! executions run a handful of commands against a near-empty book).
//!
//! Usage: `seeded_runner <seed> <operations>`

use std::env;
use std::time::Instant;

use rand::rngs::StdRng;
use rand::SeedableRng;

use fuzz::{Generator, Harness, FUZZ_CONFIG};

fn main() {
    let args: Vec<String> = env::args().collect();
    let seed: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let operations: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1_000_000);

    println!("seeded_runner: seed={seed} operations={operations} config={FUZZ_CONFIG:?}");

    let mut rng = StdRng::seed_from_u64(seed);
    let mut generator = Generator::new(FUZZ_CONFIG);
    let mut harness = Harness::new(FUZZ_CONFIG);

    let start = Instant::now();
    let progress_interval = (operations / 20).max(1);

    for i in 0..operations {
        let cmd = generator.next_command(&mut rng);
        if let Err(failure) = harness.apply(cmd) {
            let elapsed = start.elapsed().as_secs_f64();
            eprintln!("\n=== DIVERGENCE ===");
            eprintln!("seed={seed} operation_index={i} elapsed={elapsed:.3}s");
            eprintln!("{failure:#?}");
            eprintln!("\nacceptance rates up to failure:\n{}", generator.stats.report());
            std::process::exit(1);
        }

        if (i + 1) % progress_interval == 0 {
            let elapsed = start.elapsed().as_secs_f64();
            let ops_per_sec = (i + 1) as f64 / elapsed;
            eprintln!("progress: {}/{operations} ops, {elapsed:.1}s elapsed, {ops_per_sec:.0} ops/sec", i + 1);
        }
    }

    let elapsed = start.elapsed().as_secs_f64();
    println!("\ncompleted {operations} operations in {elapsed:.1}s ({:.0} ops/sec)", operations as f64 / elapsed);

    print!("replay check (fresh engines fed the full {operations}-command log)... ");
    let replay_start = Instant::now();
    match harness.check_replay() {
        Ok(()) => println!("ok ({:.1}s)", replay_start.elapsed().as_secs_f64()),
        Err(failure) => {
            eprintln!("\n=== REPLAY MISMATCH ===");
            eprintln!("seed={seed} operations={operations}");
            eprintln!("{failure:#?}");
            std::process::exit(1);
        }
    }

    println!("\n=== acceptance rates ===");
    println!("{}", generator.stats.report());
    println!("total commands accounted for: {}", generator.stats.total_commands());
    println!("\nzero divergences, zero invariant violations across {operations} operations, seed={seed}");
}
