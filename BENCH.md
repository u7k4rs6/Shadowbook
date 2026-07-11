# Bench

Tail latency for the optimized engine, at production scale (a 65,536-tick band with a 65,536-order arena), under sustained quote stuffing, not a quiet book. A quiet book is not a market that exists. Reported: p50, p99, p99.9, p99.99, and max. No single blended figure appears anywhere in this document, deliberately: blending the orders that filled cleanly together with the orders that would have blown a risk limit produces one number that no individual order actually experienced.

Methodology: every result below comes from timestamps read with a monotonic clock into a preallocated buffer of raw nanoseconds; the histogram is built from that buffer after the run, never computed inside the measured path. Before any measurement, one million commands of sustained insert-and-cancel churn run and are discarded, so the arena's free list and the summary bitmap are already under real pressure once measurement starts, not cold. Every measured operation is interleaved with continued background churn at a separate, non-interacting price zone, so the book stays busy the entire time a measurement is being taken, not just during warmup.

## insert_no_cross

A limit order resting at a price nothing else in the run ever prices through, so it never crosses: this isolates the cost of an order that rests (an arena allocation, an intrusive-list insert, level and bitmap bookkeeping), not a match. 2,000,000 samples.

| Percentile | Latency |
|---|---|
| p50 | 386 ns |
| p99 | 667 ns |
| p99.9 | 896 ns |
| p99.99 | 5,147 ns |
| max | 33,215 ns |

p99 comfortably under one microsecond, the target this project set for itself. Optimization stopped once this was met, deliberately: the target was generous on purpose, and further tuning past it would have been procrastination against a benchmark, not real engineering.

## cancel_deep

A cancel of an order sitting in the middle of a 32-deep first-in-first-out queue at one price, not at the head or the tail. The whole reason a level is an intrusive doubly-linked list rather than a plain queue is that removing an order from anywhere in it costs the same regardless of its position; canceling only ever the head of a queue would never actually exercise that claim. 2,000,000 samples.

| Percentile | Latency |
|---|---|
| p50 | 161 ns |
| p99 | 411 ns |
| p99.9 | 606 ns |
| p99.99 | 4,111 ns |
| max | 16,383 ns |

Cheaper than insert_no_cross at every percentile, which is the expected shape: removing a node whose neighbors are already known costs less than allocating a new one and linking it in.

## sweep_five_levels

Five resting sell orders at five separate price levels, swept by a single market buy for exactly five lots. This measures a real multi-level match: the summary bitmap's scan to the next occupied level, repeated four times, plus five separate fills, not a single-level match dressed up as a sweep. 500,000 samples.

| Percentile | Latency |
|---|---|
| p50 | 1,807 ns |
| p99 | 3,383 ns |
| p99.9 | 9,407 ns |
| p99.99 | 11,919 ns |
| max | 86,271 ns |

Roughly five times insert_no_cross's cost at p50, which is the right order of magnitude for five fills plus four level transitions rather than one insert.

## The retirement window bisect

The bounded retirement window that defends against order-id reuse (see FINDINGS.md) needs to answer, on every single New order, whether an id was recently retired, without allocating in the hot path. That is why its membership check is a fixed-capacity structure rather than a bare linear scan of the window. At the small window fuzzing runs with, the difference between the two is invisible: both are fast enough that nothing measures the gap. At the window size a production-scale book actually runs with, it is not.

Measured directly, both under the identical sustained-load methodology above, changing nothing but the one membership check:

| Configuration | p50 | p99 | p99.9 | p99.99 | max |
|---|---|---|---|---|---|
| Fixed-capacity membership set (committed) | 386 ns | 667 ns | 896 ns | 5,147 ns | 33,215 ns |
| Bare linear scan of the same window | 43,711 ns | 60,767 ns | 119,935 ns | 167,935 ns | 197,119 ns |

The naive version was never part of the committed engine; it was built once, measured under the same conditions as everything else in this document, and removed. Roughly two orders of magnitude separate the two at every percentile, at the exact window size a real deployment would run with. A retirement window built the naive way would have looked correct in every test and every small-scale fuzzing run, and would have quietly become the slowest thing in the entire engine the first time anyone ran it at real scale.
