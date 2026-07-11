# Findings

## What shadowbook is

shadowbook is a single-instrument limit order book matching engine in Rust. It has an optimized engine (dense tick array indexed by absolute tick, a bitmap summary scanned via bit-scan instructions, a slotmap arena with generation-counted handles, intrusive doubly-linked lists per price level, a preallocated event buffer, zero hot-path allocation) and a reference engine (a naive book keyed by price maps, linear scan everywhere, written first and optimized only for being obviously correct at a glance).

Correctness is defined as agreement between the two engines: an identical emitted event stream after every single command, checked elementwise, plus a set of absolute invariants checked against each engine independently, plus end-of-run replay determinism via a stable digest.

The two engines exist so that one can shadow the other. Every finding below is the same discovery in a different costume: the interesting failures were never really in the engine being checked, they were in the machinery doing the checking, or in a cost nobody had measured yet.

## The unifying thesis

Every finding in this report is an instance of the same shape: an unvalidated model of state, owned by something other than the component under test.

- Both engines enforce order-ID uniqueness only among currently resting orders, by construction. Left there, a whole bug class, a late cancel landing on a reused id, would be invisible to a differential that only compares the two engines against each other, because both would resolve the reused id identically (F-001).
- A fuzzer's command generator that keeps its own list of which ids are live is a second, independent model of the book. If that model is not derived from a real one, it drifts, first in size, then in membership, and a run built on it can look enormous while testing almost nothing (F-003).
- A structure's cost can be completely invisible at one scale and dominate at another, if nobody ever measures it at both (F-004, and, discovered rather than designed around, F-005 and F-006).

A differential harness catches divergence between two implementations. Nothing in it catches divergence between the harness and reality, or between a component's assumed cost and its real one. That is where every finding below lives.

## Invariants

Referred to by number throughout this report.

- I1: the book never crosses. `best_bid` strictly below `best_ask` after every command.
- I2: quantity conservation. Total filled buy volume equals total filled sell volume across a run.
- I3: no fill violates its own limit. No buy fills above its limit price, no sell below.
- I4: fills occur at the resting (maker) price. Price improvement accrues to the taker.
- I5: price priority. An incoming order exhausts the best price level before any worse level.
- I6: time priority within a level. At equal price, the lower sequence number fills first.
- I7: live-set integrity, checked bidirectionally. Every order reachable by walking every level appears exactly once in the live index, and every live-index entry is reachable from a level.
- I8: arena accounting (the optimized engine only; the reference engine has no arena to account for). Live orders plus free slots equals capacity, and the arena's own occupied-slot count agrees with the live index.
- I9: replay exactness. A fresh engine fed the full command log reaches a byte-identical digest.

I2, I3, and I4 are properties of the event stream, not of a single book snapshot, and are checked by an auditor that consumes emitted events and maintains running totals against each engine's own output independently.

## Findings

### F-001: order-ID reuse is invisible to the differential by construction

Both engines enforce order-ID uniqueness only among currently resting orders: the live index is keyed by order id, and an id drops out of that index the moment its order fills or is cancelled. Left at that, an id could rest, be filled or cancelled, and be reused by a brand new order, and a late cancel meant for the original order would land on the new one instead, a real exchange bug class. Neither engine's own correctness logic would ever disagree with the other about this: both would resolve the reused id the same way, so a pure differential comparison could never see it. The bug would live in what the two engines agree is correct, not in where they disagree.

This was built as a design decision from the start on this build, not discovered by a failing run. Both engines carry a bounded retirement window, enforced identically: a New order whose id is currently live, or present in the window of recently retired ids, is rejected outright, and mutates nothing. Under fuzzing the window is deliberately small, 32 entries against a 64-order arena, so id reuse is forced constantly rather than left to chance. Across the 100,000,000-operation run described below, that rejection fired 15,000,352 times, a quarter of every New attempt, real evidence the guard is under sustained pressure and not a check that only exists in the abstract.

### F-002: the generation counter is an assertion, not a client-facing defense

The optimized engine's arena hands out generation-counted handles, which could be read as protection against a client-triggered ABA: a stale cancel reaching a reused arena slot and cancelling the wrong order. That scenario cannot occur through the public command API on this build. Cancel and Amend are order-id keyed and resolve through the live index on every call, and the live index correctly drops an order the instant it stops being live. The only way a stale arena handle could ever reach the arena is if some other, unrelated bug already existed elsewhere in level bookkeeping; the generation counter cannot be triggered by any sequence of ordinary commands.

This was confirmed two ways on this build. A test constructs the scenario directly, deliberately bypassing the command API: cancel a real order through the normal code path, let a second order take the freed arena slot (the free list is a stack, so this is deterministic), then read the first order's now-stale handle directly. The check fires. Separately, while calibrating planted bugs for the fuzzer, two different mutations that broke the live index without breaking level bookkeeping were caught by this same assertion before either mutation's own test logic even had a chance to run, confirming it catches real internal corruption sharply and early.

So the counter defends against nothing a client can express. It is an assertion against internal corruption that is unreachable through the public API, which is a stronger property than a client-facing defense would be, not a weaker one: it holds regardless of what any client ever sends.

### F-003: a fuzzer's generator needs its own oracle, or it tests almost nothing

A command generator that keeps its own list of candidate ids to target for Cancel and Amend is a second, independent model of which orders are live. If that model is not derived from a real book, it drifts: first in size, if the candidate pool has no relationship to the arena's actual capacity, then in membership, because nothing tells the generator when an order fills. A generator in that state can run for a long time and prove almost nothing: cancels and amends bounce as rejected-for-unknown-order against a pool of mostly stale ids, and the entire arena bug surface, a cancel racing a fill, sustained churn, live-set integrity in both directions, free-list reuse, goes unexercised while the run's total operation count still looks large.

This build's generator was built with a real reference engine as its own shadow oracle from the first line of its code, not adopted after a failure. Every command the generator emits is applied to the shadow first, and every targeting decision, which id to cancel, what a live order's current price and remaining quantity actually are, is read directly from the shadow's current state. There is no separate tally to drift, because the generator is not maintaining a tally; it is asking a correctly-behaving engine what is actually true right now.

The 100,000,000-operation run is the evidence this worked. Cancel succeeded 13,917,814 times out of 25,007,556 attempts, 55.7 percent, not the low single digits a drifted model produces. Amend succeeded 11,730,112 times out of 14,997,339 attempts, 78.2 percent. Both numbers are what a generator whose liveness model tracks the truth looks like.

### F-004: a structure's cost can be invisible at one scale and dominant at another

The retirement window's membership check needs to answer whether an id is in the window without allocating in the hot path, which is why it is a fixed-capacity structure rather than a bare scan. At fuzz scale, a 32-entry window, the difference between a constant-time lookup and a linear scan is invisible: both are fast enough that nothing measures the gap. At production scale it is not.

Measured directly on this build, both under the same sustained-load methodology described in the benchmark: with the real, fixed-capacity membership check, `insert_no_cross` reports p50 386 nanoseconds and p99 667 nanoseconds. Swapping only that membership check for a bare linear scan of the same window, with nothing else changed, moves those same percentiles to p50 43,711 nanoseconds and p99 60,767 nanoseconds, roughly two orders of magnitude slower, at the exact window size a production-scale book actually runs with. The naive version was never in the committed engine; this comparison exists specifically to make its avoided cost real and visible for this build, rather than trusted on faith.

### F-005: a membership set can keep growing even when it is never allowed to hold more than it was reserved for

The retirement window's membership check was first implemented on a standard hash set, reserved to the window size up front, with an eviction always performed before an insert once at capacity, exactly the discipline that should make a structure never grow past its reservation. Under a test built specifically to assert zero allocations across ten million sustained hot-path commands, the engine allocated once anyway. Isolating the structure alone in a standalone probe traced it precisely: after roughly 150,000 insert-and-evict cycles, the set's own reported capacity had grown from a requested 16,384 to 57,344, with no growth in actual occupancy at any point. The underlying hash table's tombstone accounting forces a resize as removals accumulate, even when live occupancy never exceeds the reservation; bounding how many items are ever inserted turned out not to be the same guarantee as bounding the table itself.

The fix is a small, hand-rolled fixed-capacity structure using backward-shift deletion: removing an entry immediately slides later entries back into the freed slot rather than leaving anything behind, so there is nothing for churn to accumulate and no code path exists that could ever grow it. After the fix, the same test passes cleanly across the full ten million commands, zero allocations. The fix lives in the code shared by both engines, so the reference engine inherited it at no cost to itself.

### F-006: a clone kept for one throwaway use should not carry a run's entire history

A fill-or-kill order has to know, before committing to anything, whether it can be filled completely; the reference engine answers this with a dry run that clones the whole book, runs the real matching code against the clone, and discards it. Sharing the real matching code with the dry run is deliberate: it is the only way to guarantee the dry run applies the same self-match policy as a real execution would. The book's clone was already total by design, for a good reason: a shallower clone had already been identified as a way to reintroduce a self-match bug inside the exact mechanism meant to prevent one. That totality included the book's own command log, kept for replay and audit convenience, which nothing about matching semantics has ever needed.

Under sustained fuzzing this made every fill-or-kill order cost something proportional to the number of commands already applied, because the log being cloned kept growing for the entire life of the book. The effect was invisible at small scale and severe at real scale: the same seeded runner measured 265,904 operations per second at 10,000 commands processed, and 15,906 operations per second by 1,000,000 commands, on a book whose live-order count stayed bounded by the arena's own capacity the entire time, with no explanation available for a slowdown that tracked total commands processed rather than book size. Left as it was, a 100,000,000-operation run would not have finished in any practical time.

The fix is a manual clone that carries every field bearing on matching semantics (price levels, the live index, the retirement window, the sequence counter) and deliberately excludes the log, which the dry run's shadow copy never reads before it is discarded. The 265,904-operations-per-second figure above, at 10,000 commands, was itself measured before this fix; at that scale the log had not yet grown large enough for its cost to show. After the fix, throughput no longer degraded with scale: separately re-measured at 1,000,000 commands and at 10,000,000, and again across the full 100,000,000-operation run reported below, all three landed in the same narrow band the small-scale number had already shown, between 315,000 and 325,000 operations per second, rather than continuing to fall as the log grew. These are three separate runs at three separate scales, not one continuous measurement; what they share is the range, not a single run's timeline.

## Calibration

A fuzzer that finds nothing has proven nothing until it is shown to have teeth. Five bugs were planted, one per required tier plus two aimed specifically at the event-comparison mechanism, each confirmed caught and then reverted before the clean run below was trusted.

- Shallow: the post-only crossing check's comparison changed from "at or through the touch" to "strictly through it," an off-by-one at the exact price. Caught at operation 287, in under a millisecond.
- Medium: an amend that only decreases quantity forced to lose time priority and requeue to the back of its level, the same as a price change or a quantity increase would. Caught at operation 62, in under a millisecond.
- Deep: a level emptied by a cancel, specifically not by a fill, correctly clears its own linked list but leaves its summary bit set, a phantom touch that nothing ever crosses, so no fill diverges, but post-only rejection depends on the touch. The generation counter's removal was deliberately not used as this tier's bug, since F-002 already establishes it is not observable through the public API at all, and a fuzzer cannot catch what no command sequence can reach. Caught at operation 12, in under a millisecond, and caught three separate ways: by a snapshot check that compares the summary bitmap against actual level occupancy directly, by a defensive check inside the matching loop firing when a later order tried to walk into the phantom level, and, confirmed with both of those disabled, by the exact mechanism the bug's own shape describes: a post-only order landing precisely at the phantom price, rejected by the optimized engine and accepted by the reference engine, a genuine event-stream divergence with nothing else different between them.
- Event ordering, first: a repricing amend that crosses reports its Fill event before its own Amended event, reversed from the correct order. Final book state, the replay digest, and every invariant are identical between the two engines; only the event order differs. Caught at operation 78, in under a millisecond. This is catchable only by comparing event streams elementwise, since nothing about the book's state ever diverges.
- Event ordering, second: the reported "lost priority" flag on an Amended event computed from a price change alone, ignoring a quantity increase at an unchanged price. The order still correctly loses its place in the queue; only the reported flag is wrong. Caught at operation 134, in 2 milliseconds.

All five were reverted immediately after confirmation. None depended on a debug-only assertion for detection: two of the five were separately re-run under a release build with debug assertions off, and both still reddened identically.

## Differential run history

One seeded run is authoritative for the exit criterion, and it did not need a failed predecessor to be trusted first: because the generator's liveness model is exact by construction (F-003), there was no early run to discover was void before this one meant anything.

Seed 42, 100,000,000 operations, a 256-tick band with a 64-order arena and a 32-entry retirement window: 314.8 seconds, 317,642 operations per second, zero divergences, zero invariant violations against either engine. Replaying the full 100,000,000-command log into fresh instances of both engines matched byte for byte, checked in 20.5 seconds.

Acceptance across the run: New, 59,995,105 attempted, 65.4 percent accepted (rejected: unfillable 1,786,587, duplicate id 2,101,989, duplicate order id 15,000,352, would-cross 1,872,826; kind mix limit 35,990,437, post-only 9,002,245, market 5,997,567, immediate-or-cancel 6,000,566, fill-or-kill 3,004,290). Cancel, 25,007,556 attempted, 55.7 percent succeeded (rejected as unknown: 11,089,742). Amend, 14,997,339 attempted, 78.2 percent succeeded, 9,065,488 losing priority and 2,664,624 keeping it, 4.5 percent of all amends crossing (rejected: invalid quantity 1,325,557, unknown order 1,755,301, would-cross 186,369).

A second runner exercises the identical harness at small case size, up to 200 commands per case, specifically for shrinking; confirmed against a planted bug, it reduced a 200-step failing case down to a 2-step minimal reproduction automatically. A third, deliberately New-heavy configuration (New 80 percent, Cancel 10, Amend 10, a wider id pool, arena capacity still 64) exists specifically to reach arena exhaustion, which the balanced run above never does; over 1,000,000 operations it produced 11,656 arena-full rejections, confirmed identical in both engines, with zero divergences and zero invariant violations.

A coverage-guided volume fuzzer is unavailable in this environment (not installed, and its required system compiler is also missing), so it is not a fourth runner here. Its executions would not be summed with the seeded runner's operation count regardless: that style of fuzzer grows its inputs from empty, so most of its executions run only a handful of commands against a near-empty book. It would be a smoke test, not the instrument the exit criterion is measured against.

## What this run does and does not show

The fuzzer did not find a previously unknown bug in either engine on this build. Every divergence observed during development was a mutation planted on purpose, caught, and reverted; the clean 100,000,000-operation run found nothing wrong. Stating that plainly matters: a fuzzer that has never found anything has to earn trust some other way, which is what the calibration table above is for.

Nor does a clean run mean the engine is correct. What the evidence actually supports: across 100,000,000 operations, generated by a distribution whose liveness model is exact by construction and calibrated against five planted bugs at three depths plus two aimed specifically at event ordering, all caught, the optimized engine could not be distinguished from a reference engine written first, independently, and kept deliberately simple enough to trust by inspection. That is a real, bounded claim, not a proof.

## State-space postmortem

Reached and demonstrated: the touch's immediate neighborhood (85 percent of generated prices land within three ticks of it), the band's edges and the summary bitmap's word boundaries (a fixed adversarial set of prices specifically includes the lowest ticks, the boundary ticks around the first two bitmap words, and the band's top edge), the retirement window's eviction (over fifteen million times in the main run alone), self-match prevention under the cancel-resting policy (only four accounts, so it is frequent by construction), and the fill-or-kill counting pass diverging from real execution (exercised by both the calibration bugs and the hand-written self-match scenarios).

Reached but shallow: book depth. Cancel succeeded 55.7 percent of the time in the main run, and New's acceptance was throttled hard by duplicate-id and duplicate-order-id rejections, a quarter of every attempt, so the book stayed thin throughout. Time priority always had something to check; it rarely had many resting orders at once at a single price to check it against.

Weakly reached or unreached by the balanced distribution the exit criterion is measured against, specifically: arena exhaustion. Across all 100,000,000 operations of the main run, it never fired once, not because the check is untested but because that distribution's cancel success rate keeps the book too thin to ever approach the arena's own capacity. This is not silently accepted: a second, named, New-saturating configuration exists specifically to reach it, bounded to 1,000,000 operations, and does, 11,656 times, confirmed identical in both engines. The main run's exit criterion stands on its own terms; the gap it leaves is closed by a second instrument built for exactly that gap. Also weakly reached: the full production-scale tick band. The benchmark runs at that scale; the fuzzer, deliberately, does not, since fuzzing at that scale would trade operation volume for a wider band the interesting states do not actually need.

Structural blind spot, stated as the method's shape rather than an apology: a differential harness cannot see a bug present identically in both engines. Nothing about comparing two engines' output to each other can catch a mistake both of them make the same way. The absolute invariants, and a dedicated test that checks notional-value widening directly rather than through the differential, are the only defense against that, and together they cover quantity conservation, limit adherence, maker-side pricing, live-set integrity in both directions, and, for the optimized engine, arena accounting. Nothing checks whether the specification itself is right. That is a property of the method, not a fixable gap in this build.
