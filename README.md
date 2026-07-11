# shadowbook

Read [FINDINGS.md](FINDINGS.md) first. That is the actual deliverable; the code exists to make it true.

shadowbook is a single-instrument limit order book matching engine in Rust: an optimized engine (dense tick array, bitmap-accelerated level lookup, a slotmap arena with generation-counted handles, intrusive per-level linked lists, an id-to-handle index built to the same zero-allocation standard as everything else in it) checked continuously against a naive reference engine written first and kept deliberately simple enough to trust by inspection. Correctness is defined as agreement between the two: an identical event stream after every single command, a set of absolute invariants checked against each engine independently, and end-of-run replay determinism.

The optimized engine's hot path is verified allocation-free by a test wrapping the global allocator in a counter, covering sustained insert-and-cancel churn at low occupancy and sustained churn held near the arena's full capacity. That second case matters more than it sounds: a structure reserved to a capacity at construction is not automatically a structure that stays that size, which is exactly what FINDINGS.md's F-005 is about.

The engine is the vehicle. [FINDINGS.md](FINDINGS.md) is the product: what the differential fuzzer's own generator needed in order to test anything real, what a client-facing-looking safety mechanism turned out to actually guard against, and two real performance bugs a sustained fuzzing run surfaced by degrading rather than crashing. [ERRATA.md](ERRATA.md) records what testing corrected that planning did not catch in advance. [BENCH.md](BENCH.md) reports tail latency under load, including a direct, measured comparison against a naive version of one internal structure, to make its avoided cost real rather than assumed.

## Layout

```
reference/   the oracle. naive, obviously correct, never optimized.
engine/      the optimized engine reference specifies.
types/       the shared Command/Event vocabulary both engines speak.
fuzz/        the differential harness, its generator, and both runners.
benches/     tail latency under sustained load, at production scale.
```

## Running it

```
cargo test --workspace
cargo bench -p benches
cargo run --release -p fuzz --bin seeded_runner -- <seed> <operations>
```

The last command reproduces any run described in FINDINGS.md from its seed. A divergence, if one is ever found, prints the seed and the exact operation index it happened at.

## Scope

No networking, no persistence, no multi-instrument, no margin or liquidation, no fees beyond the post-only reject rule, no multi-threaded matching. The matching thread is single, deliberately: a matching engine is a state machine, and the moment two threads can mutate the book, the event log stops being replayable, which is the one property everything else in this project depends on.
