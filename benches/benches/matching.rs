//! Tail latency for `insert_no_cross`, `cancel_deep`, and
//! `sweep_five_levels`, at production scale (`types::BENCH_CONFIG`:
//! 65536 ticks, 65536-order arena), under sustained quote stuffing, not
//! a quiet book. `harness = false`: this is a plain binary, not a
//! criterion-statistics benchmark, because the reported numbers are tail
//! percentiles from a full latency histogram, not a mean-centered
//! statistical estimate of one operation's typical cost, which is a
//! different kind of measurement than criterion's own harness produces.
//! `std::hint::black_box` still does the one job a benchmark harness
//! needs here: stopping the compiler from optimizing away work whose
//! result is never read.
//!
//! Methodology: timestamps are read with `Instant::now()` into a
//! preallocated `Vec<u64>` of raw nanoseconds; the histogram is built
//! from that vector after the run, never computed inside the measured
//! path. Warmup of 10^6 commands, discarded. Reported: p50, p99, p99.9,
//! p99.99, max. Never a mean: a mean is the average of the orders that
//! filled cleanly and the orders that would have blown a risk limit, and
//! averaging them together produces a number no order ever experienced.

#![forbid(unsafe_code)]

use std::hint::black_box;
use std::time::Instant;

use hdrhistogram::Histogram;

use engine::Book;
use types::{Command, Kind, Side, BENCH_CONFIG};

const WARMUP_COMMANDS: u64 = 1_000_000;

const STUFF_PRICE: i64 = 30_000;
const INSERT_NO_CROSS_PRICE: i64 = 10_000;
const CANCEL_DEEP_PRICE: i64 = 20_000;
const SWEEP_BASE_PRICE: i64 = 40_000;

/// One churn cycle at a price zone nothing else ever touches: a resting
/// buy and a resting sell, both cancelled immediately. Keeps the arena's
/// free list and the summary bitmap under continuous pressure -- the A8
/// condition -- without ever crossing (30000 < 30001, so the buy never
/// reaches the sell) and without leaking state across iterations.
fn stuff(book: &mut Book, next_id: &mut u64) {
    let id1 = *next_id;
    *next_id += 1;
    book.apply(Command::New { id: id1, account: 5, side: Side::Buy, kind: Kind::Limit, price: STUFF_PRICE, qty: 1 });
    let id2 = *next_id;
    *next_id += 1;
    book.apply(Command::New { id: id2, account: 6, side: Side::Sell, kind: Kind::Limit, price: STUFF_PRICE + 1, qty: 1 });
    book.apply(Command::Cancel { id: id1 });
    book.apply(Command::Cancel { id: id2 });
}

fn warmup(book: &mut Book, next_id: &mut u64) {
    let cycles = WARMUP_COMMANDS / 4;
    for _ in 0..cycles {
        stuff(book, next_id);
    }
}

/// A Limit order resting at a price nothing else ever prices through: it
/// never crosses, so this measures the pure cost of an insert that rests
/// (arena alloc, intrusive push_back, level/bitmap bookkeeping), not a
/// match.
fn bench_insert_no_cross(book: &mut Book, next_id: &mut u64, iterations: u64) -> Histogram<u64> {
    let mut hist = Histogram::<u64>::new(3).expect("histogram");
    for _ in 0..iterations {
        stuff(book, next_id);

        let id = *next_id;
        *next_id += 1;
        let start = Instant::now();
        black_box(book.apply(Command::New { id, account: 1, side: Side::Buy, kind: Kind::Limit, price: INSERT_NO_CROSS_PRICE, qty: 1 }));
        let elapsed_ns = start.elapsed().as_nanos() as u64;
        hist.record(elapsed_ns).expect("record");

        book.apply(Command::Cancel { id });
    }
    hist
}

/// Cancels an order from the middle of a `queue_depth`-deep FIFO queue at
/// one price, not the head or tail: the intrusive doubly-linked list's
/// entire reason to exist is O(1) removal from anywhere in a level
/// without a scan, and a head-only cancel would not exercise that.
fn bench_cancel_deep(book: &mut Book, next_id: &mut u64, iterations: u64, queue_depth: usize) -> Histogram<u64> {
    let mut hist = Histogram::<u64>::new(3).expect("histogram");
    for _ in 0..iterations {
        stuff(book, next_id);

        let mut ids = Vec::with_capacity(queue_depth);
        for _ in 0..queue_depth {
            let id = *next_id;
            *next_id += 1;
            book.apply(Command::New { id, account: 2, side: Side::Sell, kind: Kind::Limit, price: CANCEL_DEEP_PRICE, qty: 1 });
            ids.push(id);
        }

        let target = ids[queue_depth / 2];
        let start = Instant::now();
        black_box(book.apply(Command::Cancel { id: target }));
        let elapsed_ns = start.elapsed().as_nanos() as u64;
        hist.record(elapsed_ns).expect("record");

        for id in ids {
            if id != target {
                book.apply(Command::Cancel { id });
            }
        }
    }
    hist
}

/// Five resting asks at five distinct price levels, swept by one Market
/// buy for exactly five lots: measures the cost of a multi-level sweep,
/// the bitmap-scan-to-next-level cost repeated four times plus five
/// fills, not a single-level match.
fn bench_sweep_five_levels(book: &mut Book, next_id: &mut u64, iterations: u64) -> Histogram<u64> {
    let mut hist = Histogram::<u64>::new(3).expect("histogram");
    for _ in 0..iterations {
        stuff(book, next_id);

        for i in 0..5 {
            let id = *next_id;
            *next_id += 1;
            book.apply(Command::New { id, account: 3, side: Side::Sell, kind: Kind::Limit, price: SWEEP_BASE_PRICE + i, qty: 1 });
        }

        let id = *next_id;
        *next_id += 1;
        let start = Instant::now();
        black_box(book.apply(Command::New { id, account: 4, side: Side::Buy, kind: Kind::Market, price: 0, qty: 5 }));
        let elapsed_ns = start.elapsed().as_nanos() as u64;
        hist.record(elapsed_ns).expect("record");
    }
    hist
}

fn report(name: &str, hist: &Histogram<u64>) {
    println!("{name}:");
    println!("  p50:    {} ns", hist.value_at_quantile(0.50));
    println!("  p99:    {} ns", hist.value_at_quantile(0.99));
    println!("  p99.9:  {} ns", hist.value_at_quantile(0.999));
    println!("  p99.99: {} ns", hist.value_at_quantile(0.9999));
    println!("  max:    {} ns", hist.max());
    println!("  samples: {}", hist.len());
}

fn main() {
    println!("BENCH_CONFIG: {BENCH_CONFIG:?}");
    let mut book = Book::new(BENCH_CONFIG);
    let mut next_id = 1u64;

    print!("warming up ({WARMUP_COMMANDS} commands, discarded)... ");
    let warmup_start = Instant::now();
    warmup(&mut book, &mut next_id);
    println!("done ({:.1}s)", warmup_start.elapsed().as_secs_f64());

    let insert_iterations = 2_000_000;
    print!("insert_no_cross ({insert_iterations} iterations)... ");
    let start = Instant::now();
    let insert_hist = bench_insert_no_cross(&mut book, &mut next_id, insert_iterations);
    println!("done ({:.1}s)", start.elapsed().as_secs_f64());

    let cancel_iterations = 2_000_000;
    let queue_depth = 32;
    print!("cancel_deep ({cancel_iterations} iterations, queue depth {queue_depth})... ");
    let start = Instant::now();
    let cancel_hist = bench_cancel_deep(&mut book, &mut next_id, cancel_iterations, queue_depth);
    println!("done ({:.1}s)", start.elapsed().as_secs_f64());

    let sweep_iterations = 500_000;
    print!("sweep_five_levels ({sweep_iterations} iterations)... ");
    let start = Instant::now();
    let sweep_hist = bench_sweep_five_levels(&mut book, &mut next_id, sweep_iterations);
    println!("done ({:.1}s)", start.elapsed().as_secs_f64());

    println!();
    report("insert_no_cross", &insert_hist);
    println!();
    report("cancel_deep", &cancel_hist);
    println!();
    report("sweep_five_levels", &sweep_hist);
}
