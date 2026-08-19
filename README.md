<img src="assets/banner.svg" alt="Shadowbook: a limit order matching engine, checked against an independent reference engine. 100,000,000 operations verified, 0 divergences, 416ns median insert latency." width="100%">

Shadowbook is a single-instrument limit order book matching engine in Rust, checked against an independent reference engine by differential fuzzing: every command is applied to both, and the two must agree on the resulting event stream, not just the resulting book.

<div align="center">
  <img alt="100,000,000 operations verified · 0 divergences · 416ns p50 insert · 0 hot-path allocations" src="assets/highlights.svg" width="100%">
</div>

Matching engines are a solved exercise. Price-time priority is a page of pseudocode, and
you can find a hundred implementations of it on GitHub before lunch. None of them can tell
you whether they are correct.

Shadowbook is not an attempt to write a fast order book. It is an attempt to write one
whose correctness is *demonstrable*: an optimized engine, a deliberately naive reference
implementation, and a differential fuzzer that has driven 100,000,000 commands through
both and found no state at which they disagree.

<div align="center">
  <img alt="types/ feeds reference/ and engine/; both feed fuzz/; engine/ also feeds benches/" src="assets/architecture.svg" width="100%">
</div>

| crate        | job                                                                          |
| ------------ | ---------------------------------------------------------------------------- |
| `types/`     | Commands and events. The only thing both implementations agree on up front.  |
| `reference/` | The oracle. `Vec`, linear scan, no cleverness. Written to be read, not run.  |
| `engine/`    | The real one. Order arena, 65,536-tick price band, zero hot-path allocation. |
| `fuzz/`      | The differential harness plus the auditor that compares emitted events.      |
| `benches/`   | Tail-latency histograms: `hdrhistogram` + `Instant`, no bench harness.       |

<div align="center">
  <img alt="A command splits into reference/ and engine/, both run it, the auditor asserts the emitted events are equal" src="assets/verification.svg" width="100%">
</div>

Every command goes to both. Every event stream gets compared. Any divergence is a bug in
exactly one of them, and the reference is the one that is obviously right.

The run that the exit criterion is measured against:

```sh
cargo run --release -p fuzz --bin seeded_runner -- 42 100000000
```

| | |
| --- | --- |
| seed | 42 |
| operations | 100,000,000 |
| divergences | **0** |
| invariant violations | **0** |
| elapsed | 314.8s |
| throughput | 317,642 ops/sec |
| replay check | byte-identical, 20.5s |

That table is a summary of what the run reported, not a transcript of it. The runner's
actual stdout is more verbose and differently shaped — a progress line every five percent,
then a full acceptance breakdown — and it is reproduced in
[`FINDINGS.md`](FINDINGS.md), which carries the complete per-command-kind counts rather
than this seven-row digest.

The counts are seed-reproducible: same seed, same operation count, same numbers on any
machine. The two timing rows are not, in the same sense — they move with the hardware and
its load — so treat 314.8s as this run on this machine, not a promised figure. The machine
is stated in [`BENCH.md`](BENCH.md) and [`FINDINGS.md`](FINDINGS.md).

<div align="center">
  <img alt="insert, no cross: median across seven runs — p50 416ns, p99 758ns, p99.9 1,593ns" src="assets/benchmark.svg" width="100%">
</div>

> [!NOTE]
> Measured on an Intel Core i5-12450HX (8C/12T), 10 GiB RAM, Ubuntu 26.04, Linux
> 7.0.0-29-generic, rustc 1.95.0, release profile, with no CPU pinning and no governor
> control, on an ordinary desktop session. These are **medians across seven runs**, not
> one run: repeating the benchmark on this machine with nothing changed moved p50 across
> 385–424 ns and p99 across 707–871 ns, so a single run's figure would be
> indistinguishable from a lucky one. The honest one-line version is "p50 around
> 400–420 ns, p99 under a microsecond, on this machine".
>
> Crossing inserts are not covered by this number. They do matching work and they are
> slower. There is no throughput benchmark in this repo, and the reciprocal of a latency
> figure is not one. Full methodology, the machine, and the raw distribution are in
> [`BENCH.md`](BENCH.md).

<div align="center">
  <img alt="Both reference/ and engine/ mishandle a reused order id, both emit the wrong event, and the auditor still reports them equal" src="assets/findings.svg" width="100%">
</div>

Once the engine stabilized, the bugs stopped being in the engine. They moved into the
verification machinery: the generator, the auditor, the shrinker. The thing I built to
find bugs became the thing with the bugs in it.

And there is a class of bug it structurally cannot see. If `reference/` and `engine/`
share an assumption, they share the bug, they emit identical wrong events, and the
auditor reports agreement. Order-id reuse was exactly this. No amount of fuzzing finds
it, because 100,000,000 green runs and 100,000,000 correct runs are not the same claim.

Zero divergences means zero *disagreement*. It has never meant zero bugs.

[`FINDINGS.md`](FINDINGS.md) · [`ERRATA.md`](ERRATA.md)

## Layout

```
shadowbook/
├── types/          commands, events, ids
├── reference/      the oracle
├── engine/         the optimized book
│   ├── arena.rs    65,536-order slab
│   └── levels.rs   65,536-tick price band, bitmap-summarized
├── fuzz/           differential harness + auditor
├── benches/        hdrhistogram + Instant, harness = false
├── FINDINGS.md     what the fuzzer found, including its own bugs
├── BENCH.md        methodology and raw distributions
└── ERRATA.md       claims I corrected after making them
```

## Running

```sh
cargo test --workspace                                          # unit + reference conformance
cargo run --release -p fuzz --bin seeded_runner -- 42 1000000   # differential, short
cargo run --release -p fuzz --bin seeded_runner -- 42 100000000 # differential, the full run
cargo bench -p benches                                          # tail-latency histograms
```

`seeded_runner` reproduces any run described in FINDINGS.md from its seed. A divergence, if one is ever found, prints the seed and the exact operation index it happened at.

## Scope

No networking, no persistence, no multi-instrument, no margin or liquidation, no fees beyond the post-only reject rule, no multi-threaded matching. The matching thread is single, deliberately: a matching engine is a state machine, and the moment two threads can mutate the book, the event log stops being replayable, which is the one property everything else in this project depends on.
