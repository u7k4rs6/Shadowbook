# Shadowbook

Shadowbook is a single-instrument limit order book matching engine in Rust, checked against an independent reference engine by differential fuzzing: every command is applied to both, and the two must agree on the resulting event stream, not just the resulting book.

What sets this apart from another order-book rebuild is not the engine, it is what testing it turned up. Across 100,000,000 differentially fuzzed operations, zero divergences, a real result but a bounded one: evidence that nothing in the state space this run reached could tell the optimized engine apart from the reference, not a proof the engine is correct. And every real flaw actually found, during the original build and again during a later adversarial audit, was in the machinery checking the engine, not in the engine itself. A whole class of order-id-reuse bug is invisible to a differential by construction, because both engines make exactly the same mistake and agree on it. This repo ends up being as much about what verification cannot catch as about the engine it verifies.

The concrete, checkable part: resting a new order costs a median of roughly 400ns at production scale (a 65,536-tick band, 65,536-order arena) under sustained quote stuffing, not a quiet book, with zero allocations on the hot path, verified by a test that counts every call into the global allocator. Like any latency figure, this one has normal run-to-run variance; the shape, allocation-free, sub-microsecond at p99, is the durable claim, not the third digit.

[FINDINGS.md](FINDINGS.md) is the full account of what was checked, what was found, and what actually backs that 100,000,000-operation number. [ERRATA.md](ERRATA.md) records what testing corrected that planning did not catch in advance. [BENCH.md](BENCH.md) has the full latency tables, including a direct measured comparison against a naive version of one internal structure. If you want the depth, it's there.

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
