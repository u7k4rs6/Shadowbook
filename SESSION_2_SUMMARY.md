# Session 2: corrections, mutation calibration, event auditor

> **Historical session log.** This file records the state of the build at the end of
> one working session and is not maintained against the current tree. Numbers,
> counts and "what's still true" notes below were accurate when written and have
> since moved on. For the current state see [`FINDINGS.md`](FINDINGS.md),
> [`BENCH.md`](BENCH.md) and [`ERRATA.md`](ERRATA.md).

## Corrections applied (items 1-6 from the review)

1. **Event ordering pinned.** `Amended` is emitted before any `Fill`/`Cancelled`
   events it causes, never interleaved (`handle_amend` in `reference/src/lib.rs`).
   This was already the code's behavior; it's now documented as spec, not
   incidental ordering.
2. **`kind` sticky on `Order`.** Already present on `Order` since Session 1
   (`pub kind: Kind`); what was missing was the invariant. Added
   `check_only_resting_kinds` to `invariants.rs`: `Market`/`Ioc`/`Fok` must
   never be found resting, since they never rest by definition.
3. **Price band pinned.** `N_TICKS = 65536`, `Config::DEFAULT_CONFIG = { tick_min:
   0, tick_max: 65535 }`, absolute ticks, no offset arithmetic. `RejectReason::
   InvalidPrice` renamed to `PriceOutOfBand` (it had exactly one call site --
   the band check -- so this is a rename, not an addition alongside a
   now-dead variant).
4. **Sequencing fixed.** `RefBook::apply` now assigns `seq` once per command,
   at the top, before dispatching to `handle_new`/`handle_cancel`/
   `handle_amend`, and before any accept/reject decision. Every command
   consumes a sequence number, including ones that get rejected.
   `Event::Rejected` now carries `seq`. A2's test now asserts the concrete
   premise: the rejected cancel's `seq` is exactly two past the resting
   order's (one for the New that filled it, one for itself), which is only
   statable now that rejects consume sequence space.
5. **Zero-quantity amend rejects `InvalidQuantity`**, matching zero-quantity
   `New`  - already true in Session 1's code, now stated as a decision rather
   than an artifact.
6. **FOK rollback documented as a decision.** Added to `compute_fillable`'s
   doc comment: a FOK that rejects rolls back its self-match cancellations
   because the clone that performed them is discarded. `a4_fok_dry_run_
   matches_self_match_policy` sub-case (b) asserts this directly: after a FOK
   is rejected as unfillable, the self-match bait order is still live and
   resting in the real book.

## Task 1: mutation calibration

Each mutation was applied to a clean tree, the single named test was run in
isolation, the result recorded, then reverted with `git checkout`. All ten
went red.

| Scenario | Mutation | Test went red |
|---|---|---|
| A1 | `would_cross` (Buy): `ask <= price` → `ask < price` (off-by-one at the exact touch) | **Yes**  - assertion failure, post-only at the touch was wrongly accepted |
| A2 | `run_match`: on a fully-filled maker, stop removing it from `live` | **Yes**  - assertion failure, the racing cancel found the id and cancelled it instead of rejecting `UnknownOrder` |
| A3 | Self-match branch condition changed to `if false && front_account == taker_account` | **Yes**  - assertion failure, account 1 filled against itself |
| A4 | `compute_fillable` replaced with a naive sum of resting quantity per level (no self-match simulation) | **Yes**  - assertion failure on sub-case (b): an order whose only liquidity was self-match bait was wrongly `Accepted` instead of `Rejected(Unfillable)` |
| A5 | `let Some(price) = best_price else { break }` → `let price = best_price.unwrap()` | **Yes**  - panic (`unwrap` on `None`) on a market order into an empty book |
| A6 | `notional`: multiply in `i64` (`price.wrapping_mul(qty as i64)`) before widening to `i128` | **Yes**  - assertion failure, wrapped value didn't match the true `i128` product |
| A7 | `run_match`: push the same `Fill` event twice | **Yes**  - assertion failure, `buy_filled`/`sell_filled` doubled to 20,000 |
| A8 | `handle_cancel`: remove from `live` but skip `remove_order` (leaves the order in its level deque) | **Yes**  - I7 violation ("in the bid book but not in the live index") caught by `assert_no_violations` |
| A9 | `drop_level_if_empty` turned into a no-op | **Yes**  - panic (`unwrap` on an empty deque's `front()`), since a stale empty level made `best_ask`/`best_bid` report a price with no orders behind it |
| A10 | `Config::contains`: `price >= tick_min` → `price > tick_min` (excludes the low edge) | **Yes**  - assertion failure, tick 0 was wrongly rejected as `PriceOutOfBand` |

All ten are red under their own mutation. None were decoration.

Two mutations (A5, A9) surfaced as panics rather than clean assertion
failures. Both are still valid detections (`cargo test` reports the test as
FAILED either way), and in both cases the panic is *itself* informative: it's
exactly the failure mode the test plan warns about (A5: "check the
`best_ask: Option<TickIdx>` unwrap path"; A9: "an off-by-one walks off the
end of the summary array"). A9's mutation doesn't hit the array-walk version
of that bug -- there's no summary bitmap in `reference` -- but it hits the
`BTreeMap` analogue: a level structure that isn't actually reclaimed when
its liquidity is gone.

## Task 2: event auditor (I2, I3, I4)

Added `types::EventAuditor`, consuming `(cmd: &Command, events: &[Event])`
pairs in order and maintaining running totals. Lives in `types` so `engine`
audits against the same implementation once it exists, rather than a second
one that can drift.

- **I2** (quantity conservation): `buy_filled`/`sell_filled` are credited
  independently per fill, from each side's *recorded* identity (captured at
  `Accepted`/`Amended` time from the original command) rather than assumed
  from the `Fill` event's shape. This makes the check non-tautological: if a
  bug ever let a maker and taker land on the same side, the totals would
  diverge instead of silently doubling a number nothing cross-checked.
- **I3** (no fill violates its limit): checked for both maker and taker on
  every `Fill`, against each order's currently-recorded limit price (updated
  on `Amended`). This is "the promise the word limit makes," and Session 1
  left it completely unchecked outside of a `debug_assert!` inside
  `run_match` itself (which only catches it in debug builds and only for the
  maker's own crossing check, not independently). It's now checked
  independently, from the outside, for both sides.
- **I4** (fills at the maker's price): checked against the maker's recorded
  resting price at fill time.
- **I8** unchanged: still N/A to `reference`, still applies to `engine` only.

`a7_no_dust_across_many_partial_fills` now runs through `EventAuditor`
instead of hand-rolled counters, so it's exercising the same machinery the
Day 3 fuzz harness will use, not a parallel implementation of the same idea.

## Task 3: sequencer + sticky kind

Both done as part of the corrections above. `apply()` is now the single
place `next_seq` is read or incremented; `handle_new`/`handle_cancel`/
`handle_amend` all take `seq` as a parameter rather than touching the
counter themselves, which makes "every command consumes exactly one
sequence number, assigned before validation" a structural property of the
code rather than something each handler has to remember to do correctly.

## Task 4: amend semantics + clone totality

- **`new_qty` is the new remaining quantity**, compared against `current.
  remaining` (this was already what the Session 1 code did --
  `qty_increased = new_qty > current.remaining` -- but it wasn't stated as a
  decision anywhere). `amend_qty_is_against_remaining_not_total` now tests
  this explicitly with a partially-filled order (40 of 100 filled, 60
  remaining) and both directions: `new_qty=50` (below remaining, keeps
  priority) and `new_qty=55` (above remaining but still below the original
  qty of 100 -- loses priority under the "compare to remaining" rule, which
  would *keep* priority under a wrong "compare to original total" rule). The
  test confirms the resulting FIFO order via an actual sweep, not just the
  `lost_priority` flag.
- **`RefBook::clone()` is total.** It already was, structurally --
  `#[derive(Clone)]` on the whole struct clones every field, including
  `live` and `next_seq`, not just the price maps. Documented explicitly on
  the struct and verified with `clone_is_total_and_independent`, which
  clones a book, mutates the original, and asserts the clone doesn't observe
  it (and vice versa) -- proving independence, not just field coverage.

## What's still true from Session 1

`engine/` and `fuzz/` remain untouched placeholder crates. Full workspace
build is warning-free; `cargo test --workspace` is green (14 tests in
`reference`, `types`/`engine`/`fuzz` have none yet).

That count is this session's, and is left as written rather than updated: it
describes a tree in which `engine/` and `fuzz/` contained no logic at all. It is
not a claim about the repository as it stands. `cargo test --workspace` on the
current tree runs **38** tests: 19 in `reference`, 15 in `engine` (14 attack
scenarios plus one unit test in `src/lib.rs`), the zero-allocation test, and three
in `fuzz` (arena saturation, the non-differential notional test, and the proptest
runner).
