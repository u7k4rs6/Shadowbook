# Bench

Tail latency for the optimized engine, at production scale (a 65,536-tick band with a 65,536-order arena), under sustained quote stuffing, not a quiet book. A quiet book is not a market that exists. Reported: p50, p99, p99.9, p99.99, and max. No single blended figure appears anywhere in this document, deliberately: blending the orders that filled cleanly together with the orders that would have blown a risk limit produces one number that no individual order actually experienced.

Methodology: every result below comes from timestamps read with a monotonic clock, one reading immediately before and one immediately after the single `Book::apply` being measured, with nothing else inside the timed region; the sample is filed into an `hdrhistogram` only after the elapsed time has already been read, so histogram bookkeeping is never part of what is measured. Before any measurement, one million commands of sustained insert-and-cancel churn run and are discarded, so the arena's free list and the summary bitmap are already under real pressure once measurement starts, not cold. Every measured operation is interleaved with continued background churn at a separate, non-interacting price zone, so the book stays busy the entire time a measurement is being taken, not just during warmup.

## Machine

Every latency figure in this document was measured on:

| | |
|---|---|
| CPU | Intel Core i5-12450HX, 8 cores / 12 threads, 800-4400 MHz |
| RAM | 10 GiB |
| OS | Ubuntu 26.04 LTS, Linux 7.0.0-29-generic |
| Toolchain | rustc 1.95.0 (59807616e 2026-04-14), release profile |
| Command | `cargo bench -p benches` |

No CPU pinning, no governor control, no attempt to quiet the machine: the stock
`powersave` governor, SMT enabled, turbo left entirely to the kernel, and an ordinary
interactive desktop session running alongside. A latency number without a machine
beside it is not reproducible, so the machine is stated here rather than left implied.

### Why every table below reports a range

One benchmark run is not a measurement. Repeating `cargo bench -p benches` on this
machine, changing nothing, moved p50 between 385 ns and 424 ns and p99 between 707 ns
and 871 ns -- roughly ten percent on p50 and over twenty on p99, from nothing but
scheduler preemption, frequency scaling and whatever else the desktop was doing. The
deep tail moves far more than that: p99.9 ranged from 936 ns to 7,815 ns across the
same runs.

So no table below reports one run. Each figure is the **median across seven runs** of
`insert_no_cross` and six of the other two, with the full observed range beside it. A
single run's numbers would be indistinguishable from a lucky one, and publishing the
best of several would be worse than publishing none. The bolded p50 and p99 rows are
the load-bearing numbers; p99.9 and below are reported because hiding them would be
dishonest, not because they are stable.

The honest summary of this benchmark is therefore "p50 around 400-420 ns and p99 under
a microsecond on this machine", not any single value to three significant figures.

## insert_no_cross

A limit order resting at a price nothing else in the run ever prices through, so it never crosses: this isolates the cost of an order that rests (an arena allocation, an intrusive-list insert, level and bitmap bookkeeping), not a match. 2,000,000 samples.

| Percentile | Median | Observed range |
|---|---|---|
| p50 | **416 ns** | 385 ns – 424 ns |
| p99 | **758 ns** | 707 ns – 871 ns |
| p99.9 | 1,593 ns | 936 ns – 7,815 ns |
| p99.99 | 9,235 ns | 9,063 ns – 27,631 ns |
| max | 242,815 ns | 199,807 ns – 361,727 ns |

p99 at a median of 758 ns and never worse than 871 ns across seven runs, under the one-microsecond target this project set for itself. Optimization stopped once this was met, deliberately: the target was generous on purpose, and further tuning past it would have been procrastination against a benchmark, not real engineering.

## cancel_deep

A cancel of an order sitting in the middle of a 32-deep first-in-first-out queue at one price, not at the head or the tail. The whole reason a level is an intrusive doubly-linked list rather than a plain queue is that removing an order from anywhere in it costs the same regardless of its position; canceling only ever the head of a queue would never actually exercise that claim. 2,000,000 samples.

| Percentile | Median | Observed range |
|---|---|---|
| p50 | **148 ns** | 146 ns – 159 ns |
| p99 | **398 ns** | 390 ns – 424 ns |
| p99.9 | 558 ns | 526 ns – 685 ns |
| p99.99 | 6,669 ns | 4,591 ns – 7,699 ns |
| max | 205,759 ns | 71,743 ns – 364,031 ns |

Cheaper than insert_no_cross at every percentile, which is the expected shape: removing a node whose neighbors are already known costs less than allocating a new one and linking it in.

## sweep_five_levels

Five resting sell orders at five separate price levels, swept by a single market buy for exactly five lots. This measures a real multi-level match: the summary bitmap's scan to the next occupied level, repeated four times, plus five separate fills, not a single-level match dressed up as a sweep. 500,000 samples.

| Percentile | Median | Observed range |
|---|---|---|
| p50 | **1,804 ns** | 1,749 ns – 1,965 ns |
| p99 | **3,175 ns** | 2,907 ns – 3,351 ns |
| p99.9 | 7,867 ns | 5,771 ns – 10,271 ns |
| p99.99 | 42,679 ns | 10,511 ns – 193,791 ns |
| max | 184,127 ns | 78,655 ns – 327,167 ns |

A little over four times insert_no_cross's cost at p50, which is the right order of magnitude for five fills plus four level transitions rather than one insert.

## The retirement window bisect

The bounded retirement window that defends against order-id reuse (see FINDINGS.md) needs to answer, on every single New order, whether an id was recently retired, without allocating in the hot path. That is why its membership check is a fixed-capacity structure rather than a bare linear scan of the window. At the small window fuzzing runs with, the difference between the two is invisible: both are fast enough that nothing measures the gap. At the window size a production-scale book actually runs with, it is not.

Measured directly, both under the identical sustained-load methodology above, changing nothing but the one membership check.

Both rows below are from a single earlier session on the same machine described under
Machine above, and they are left at those original values on purpose. Only the paired
comparison carries the point, and only one of the two rows can be regenerated: the
naive membership check was deleted after it was measured, so re-running the benchmark
today re-measures the committed row and nothing else. Replacing one row with a fresh
number while the other stayed frozen would silently turn a same-session A/B into a
comparison across two different runs. The committed configuration's current numbers,
re-measured, are the insert_no_cross table above (p50 416 ns, p99 758 ns); the ~30 ns
of drift between that and this table's first row is run-to-run variance on one machine,
and is nothing next to the effect being shown here.

| Configuration | p50 | p99 | p99.9 | p99.99 | max |
|---|---|---|---|---|---|
| Fixed-capacity membership set (committed, and still the shipped one) | 386 ns | 667 ns | 896 ns | 5,147 ns | 33,215 ns |
| Bare linear scan of the same window (deleted; unregenerable) | 43,711 ns | 60,767 ns | 119,935 ns | 167,935 ns | 197,119 ns |

The naive version was never part of the committed engine; it was built once, measured under the same conditions as everything else in this document, and removed. Because it was removed, that second row cannot be regenerated from any checkout of this repository, in the same sense as F-005's and F-006's development figures in FINDINGS.md. Roughly two orders of magnitude separate the two at every percentile, at the exact window size a real deployment would run with. A retirement window built the naive way would have looked correct in every test and every small-scale fuzzing run, and would have quietly become the slowest thing in the entire engine the first time anyone ran it at real scale.
