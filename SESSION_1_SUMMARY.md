# Session 1: reference engine and harness

> **Historical session log.** This file records the state of the build at the end of
> one working session and is not maintained against the current tree. Numbers,
> counts and "what's still true" notes below were accurate when written and have
> since moved on. For the current state see [`FINDINGS.md`](FINDINGS.md),
> [`BENCH.md`](BENCH.md) and [`ERRATA.md`](ERRATA.md).

## What was built

- Cargo workspace: `types`, `reference`, `engine` (stub), `fuzz` (stub), `benches/` (placeholder dir).
- `types`: `Command`, `Event`, `Side`, `Kind`, `RejectReason`, `Config` (price band). `Event` derives `Debug`, `PartialEq` (plus `Clone`/`Copy`/`Eq`, harmless supersets).
- `reference`: `RefBook`  - `BTreeMap<Reverse<Price>, VecDeque<Order>>` bids, `BTreeMap<Price, VecDeque<Order>>` asks, `HashMap<OrderId, Side>` live index. `#![forbid(unsafe_code)]`. All five order kinds, self-match prevention (`CancelResting`), the amend priority asymmetry, `check_invariants(&RefBook)`, `digest()`/`replay_matches()` for I9.
- `reference/tests/attack_scenarios.rs`: A1-A10, hand-written, run against the reference engine only.

## A-scenario results: all 10 pass

This is the opposite of what the task brief predicted ("Several of these should FAIL right now"). That prediction assumes a first-pass naive implementation; I read the three docs closely before writing any matching code and specifically designed around the two traps the test plan calls out by name, rather than discovering them after a failing test:

- **A3/A2 (mid-walk mutation, ABA)**: `run_match` never holds an iterator across a mutation. Every loop iteration re-queries `BTreeMap::keys().next()` for the current best price fresh. Cancel-by-id removes from `live` immediately on fill, so a same-tick-later cancel for a filled id naturally falls through to `UnknownOrder`.
- **A4 (FOK dry-run divergence)**: the dry run doesn't reimplement matching  - `compute_fillable` clones the whole book and runs the *exact same* `run_match` function used for real execution, then discards the clone. Sharing the code path makes divergence structurally impossible rather than something to catch after the fact.
- **A6 (notional overflow)**: no notional computation exists anywhere in the matching path (nothing in scope needs one), so I added `reference::notional(price, qty) -> i128` as an explicit widening utility and tested it directly, per the test plan's note that this needs a dedicated non-differential test.

I'm not aware of a bug in the reference engine that these tests should have caught but didn't  - but per the test plan's own framing ("the engine is assumed wrong until the fuzzer fails to prove it"), a clean hand-written suite proves relatively little; A1-A10 are the scenarios a human thought to check, not the ones a fuzzer will find. That's Day 3's job, not this session's.

## check_invariants(&RefBook) scope

`check_invariants` takes a single book snapshot, per the task instruction. Four of the nine invariants are not properties of a snapshot and are handled elsewhere, documented in `reference/src/invariants.rs`:

- **I1, I5, I6, I7**  - checked in full from the snapshot.
- **I2** (quantity conservation) and **I3/I4** (fill price/limit correctness) are properties of an event *stream*, not a resting book. I3/I4 are additionally guarded with `debug_assert!` at the point `Fill` events are constructed in `run_match`, so they panic immediately in debug builds rather than silently passing. A running I2 total belongs in the Day 3 fuzz harness, which is the first place many commands accumulate.
- **I8** (arena accounting) doesn't apply to `reference`  - no arena exists here by design. Applies to `engine` only.
- **I9** (replay) needs two book instances, not one; exposed separately as `RefBook::replay_matches()`.

## Assumptions made and flagged for review  - please confirm or correct

These are places where section 6 of the architecture doc was silent and I made a call rather than guessing silently:

1. **Amend re-triggers matching.** Section 6 lists the priority rule but doesn't say whether a re-priced order that now crosses actually matches. I inferred it must, from architecture line 48 ("Amend is remove then re-insert") plus A3's own scenario, which only makes sense if re-insertion behaves like a fresh `Limit` order (crossing → matches, with self-match prevention applying). I'm fairly confident this reading is right, but it's an inference, not literal text.
2. **Post-only + amend.** Not covered by section 6 at all. I chose: if an amend to a `PostOnly` order's price/qty would cross, reject the *amend* (`WouldCross`) and leave the resting order untouched, rather than letting it convert into a taker. This keeps "post-only never takes" true under amend too, but it's my extrapolation, not spec text. An alternative reading: post-only orders simply can't be amended in a crossing direction and the rule is undefined/doesn't-happen. Want me to confirm this is the intended behavior?
3. **Price band (`Config`) applies to the reference engine at all.** Section 4.1 of the architecture doc describes the price band as a consequence of `engine`'s dense tick array (a memory/implementation constraint), not stated as part of the abstract command semantics in section 6. But section 7's replay pseudocode calls both `Book::new(config)` and implicitly a matching `RefBook::new(cfg)`, and A10 only makes sense as a *differential* test if both engines reject identically outside some shared band. I added `Config { tick_min, tick_max }` to `reference` and enforce it on `New`/`Amend`. This is almost certainly what's intended, but the band's actual value (tick count, reference price) is undefined until `engine`'s `N_TICKS` is chosen in Day 2  - flagging so the two don't drift apart.
4. **Ingress sequencing vs. acceptance sequencing.** Architecture section 2 says "Sequence: assigned by the engine at ingress," but `Event::Rejected` carries no `seq` field. I assign `next_seq` only on acceptance (rejected/duplicate/invalid orders never consume a sequence number). If you want every ingress command  - accepted or not  - to consume a sequence number (e.g. for audit purposes), that's a small change but changes what "seq" means for replay.
5. **Amend to `new_qty == 0`.** Not covered by section 6. I reject it (`InvalidQuantity`) rather than treating it as an implicit cancel. Flagging in case you'd rather zero-qty-amend behave like `Cancel`.

None of these were resolved by silently picking whichever made a test pass  - each was decided from the most specific text I could find in the docs, and is called out here for you to overrule.

## Not touched this session

`engine/` and `fuzz/` are placeholder crates only (doc comment, no logic), per instructions.
