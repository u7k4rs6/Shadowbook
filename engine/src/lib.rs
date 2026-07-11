//! `engine`: the optimized matching engine. An implementation of the
//! specification `reference` defines, not a second specification: any
//! disagreement between the two is resolved in favor of `reference` until
//! proven otherwise.
//!
//! Dense tick array plus bitmap summary (`levels`), slotmap arena with
//! generation-counted handles and an intrusive doubly-linked list per
//! price level (`arena`), preallocated event ring (`apply` returns a
//! slice, not a `Vec`). No `unsafe` was needed to reach the p99-under-a-
//! microsecond target, so this crate stays `#![forbid(unsafe_code)]` too.

#![forbid(unsafe_code)]

mod arena;
mod invariants;
mod levels;

pub use invariants::check_invariants;

/// `price * quantity`, widened to `i128` before multiplication, mirroring
/// `reference::notional` exactly. Nothing in the matching path computes a
/// notional figure (no fee/margin/risk-limit logic is in scope for either
/// engine), so like its reference counterpart this exists purely so A6's
/// widening rule is tested explicitly rather than assumed.
pub fn notional(price: Price, qty: Qty) -> i128 {
    (price as i128) * (qty as i128)
}

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use arena::{Handle, OrderArena, OrderNode};
use levels::Levels;
use types::{AccountId, Command, Config, Event, FixedIdMap, Kind, OrderId, Price, Qty, RejectReason, RetirementRing, Seq, Side};

/// Push `handle` (already carrying valid `id`/`side`/`price`/etc, with
/// `prev`/`next` not yet meaningful) to the back of `tick`'s intrusive
/// list. Takes `arena`/`levels` as disjoint parameters, not `&mut self`
/// methods, so `Book`'s call sites can borrow `self.arena` and
/// `self.bids`/`self.asks` as sibling fields instead of fighting the
/// borrow checker over one field owning the other.
fn push_back(arena: &mut OrderArena, levels: &mut Levels, tick: usize, handle: Handle) {
    let old_tail = levels.tail(tick);
    {
        let node = arena.get_mut(handle);
        node.prev = old_tail;
        node.next = None;
    }
    match old_tail {
        Some(t) => arena.get_mut(t).next = Some(handle),
        None => levels.set_head(tick, Some(handle)),
    }
    levels.set_tail(tick, Some(handle));
    levels.inc_count(tick);
}

/// Unlink `handle` from `tick`'s intrusive list in O(1): no scan, because
/// the node already knows its own `prev`/`next`.
fn unlink(arena: &mut OrderArena, levels: &mut Levels, tick: usize, handle: Handle) {
    let (prev, next) = {
        let node = arena.get(handle);
        (node.prev, node.next)
    };
    match prev {
        Some(p) => arena.get_mut(p).next = next,
        None => levels.set_head(tick, next),
    }
    match next {
        Some(n) => arena.get_mut(n).prev = prev,
        None => levels.set_tail(tick, prev),
    }
    levels.dec_count(tick);
}

fn pop_front(arena: &mut OrderArena, levels: &mut Levels, tick: usize) -> Option<Handle> {
    let head = levels.head(tick);
    if let Some(h) = head {
        unlink(arena, levels, tick, h);
    }
    head
}

fn hash_order_content(node: &OrderNode, hasher: &mut DefaultHasher) {
    node.id.hash(hasher);
    node.account.hash(hasher);
    node.side.hash(hasher);
    node.kind.hash(hasher);
    node.price.hash(hasher);
    node.qty.hash(hasher);
    node.remaining.hash(hasher);
    node.seq.hash(hasher);
}

pub struct Book {
    cfg: Config,
    arena: OrderArena,
    bids: Levels,
    asks: Levels,
    /// `FixedIdMap`, not `std::collections::HashMap`: a `HashMap`
    /// reserved to `capacity` up front still grows its own table under
    /// sustained insert/remove churn held near that capacity, the same
    /// hashbrown tombstone-accounting mechanism `RetirementRing` was
    /// fixed for first. See `types::FixedIdMap`'s doc comment.
    live: FixedIdMap<Handle>,
    retirement: RetirementRing,
    next_seq: Seq,
    /// Preallocated at `new`, cleared (not deallocated) at the start of
    /// every `apply`. Reserved to `2 * capacity + 8`: the worst case for
    /// a single command is a sweep through every resting order (at most
    /// `capacity` of them, each producing one `Fill` or self-match
    /// `Cancelled`) plus a small constant of bookkeeping events
    /// (`Accepted`/`Amended`/a final non-resting `Cancelled`). Since
    /// `capacity` bounds live orders by construction (`RejectReason::
    /// ArenaFull`), a single `apply` can never exceed this reservation,
    /// so `apply` never allocates.
    event_buf: Vec<Event>,
}

impl Book {
    pub fn new(cfg: Config) -> Self {
        Book {
            cfg,
            arena: OrderArena::new(cfg.capacity),
            bids: Levels::new(cfg.n_ticks),
            asks: Levels::new(cfg.n_ticks),
            live: FixedIdMap::with_capacity(cfg.capacity),
            retirement: RetirementRing::new(cfg.id_retirement_window),
            next_seq: 0,
            event_buf: Vec::with_capacity(cfg.capacity * 2 + 8),
        }
    }

    pub fn config(&self) -> Config {
        self.cfg
    }

    pub fn best_bid(&self) -> Option<Price> {
        self.bids.highest_occupied().map(|t| t as Price)
    }

    pub fn best_ask(&self) -> Option<Price> {
        self.asks.lowest_occupied().map(|t| t as Price)
    }

    pub fn is_live(&self, id: OrderId) -> bool {
        self.live.contains_key(id)
    }

    pub fn live_count(&self) -> usize {
        self.live.len()
    }

    /// A stable hash of the full book, walked in canonical priority order
    /// (best-to-worst price, FIFO within a level), hashing only the
    /// semantic order fields -- `prev`/`next` are excluded, since they
    /// are internal arena-slot bookkeeping, not book content, and two
    /// engines with identical resting orders can legitimately disagree on
    /// which arena slot holds which order.
    pub fn digest(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        "bids".hash(&mut hasher);
        let mut tick_opt = self.bids.highest_occupied();
        while let Some(tick) = tick_opt {
            (tick as Price).hash(&mut hasher);
            let mut cur = self.bids.head(tick);
            while let Some(h) = cur {
                let node = self.arena.get(h);
                hash_order_content(node, &mut hasher);
                cur = node.next;
            }
            tick_opt = self.bids.highest_occupied_before(tick);
        }
        "asks".hash(&mut hasher);
        let mut tick_opt = self.asks.lowest_occupied();
        while let Some(tick) = tick_opt {
            (tick as Price).hash(&mut hasher);
            let mut cur = self.asks.head(tick);
            while let Some(h) = cur {
                let node = self.arena.get(h);
                hash_order_content(node, &mut hasher);
                cur = node.next;
            }
            tick_opt = self.asks.lowest_occupied_after(tick);
        }
        hasher.finish()
    }

    /// Sequence is assigned here, at ingress, before matching, before the
    /// accept/reject decision even exists, mirroring `RefBook::apply`
    /// exactly: one counter, incremented on every command whether it is
    /// ultimately accepted or rejected.
    pub fn apply(&mut self, cmd: Command) -> &[Event] {
        self.event_buf.clear();
        let seq = self.next_seq;
        self.next_seq += 1;
        match cmd {
            Command::New { id, account, side, kind, price, qty } => self.handle_new(seq, id, account, side, kind, price, qty),
            Command::Cancel { id } => self.handle_cancel(seq, id),
            Command::Amend { id, new_price, new_qty } => self.handle_amend(seq, id, new_price, new_qty),
        }
        &self.event_buf
    }

    fn push_event(&mut self, e: Event) {
        self.event_buf.push(e);
    }

    // ---- New ------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn handle_new(&mut self, seq: Seq, id: OrderId, account: AccountId, side: Side, kind: Kind, price: Price, qty: Qty) {
        if qty == 0 {
            self.push_event(Event::Rejected { id, reason: RejectReason::InvalidQuantity, seq });
            return;
        }
        if self.live.contains_key(id) {
            self.push_event(Event::Rejected { id, reason: RejectReason::DuplicateId, seq });
            return;
        }
        if self.retirement.contains(id) {
            self.push_event(Event::Rejected { id, reason: RejectReason::DuplicateOrderId, seq });
            return;
        }
        // Market orders carry no meaningful price; the band does not apply.
        if kind != Kind::Market && !self.cfg.contains(price) {
            self.push_event(Event::Rejected { id, reason: RejectReason::PriceOutOfBand, seq });
            return;
        }
        // Checked at ingress, before matching, before any mutation. See
        // `RejectReason::ArenaFull`: a marketable order that would have
        // fully filled without ever resting is still rejected here if the
        // arena is full, because this check runs before matching is even
        // attempted.
        if !self.arena.has_free_slot() {
            self.push_event(Event::Rejected { id, reason: RejectReason::ArenaFull, seq });
            return;
        }

        let limit_price = match kind {
            Kind::Market => None,
            _ => Some(price),
        };

        if kind == Kind::PostOnly && self.would_cross(side, price) {
            self.push_event(Event::Rejected { id, reason: RejectReason::WouldCross, seq });
            return;
        }

        if kind == Kind::Fok {
            let fillable = self.compute_fillable(account, side, limit_price, qty);
            if fillable < qty {
                self.push_event(Event::Rejected { id, reason: RejectReason::Unfillable, seq });
                return;
            }
        }

        self.push_event(Event::Accepted { id, seq });

        let remaining = self.run_match(id, account, side, limit_price, qty);
        let ends_up_resting = remaining > 0 && matches!(kind, Kind::Limit | Kind::PostOnly);

        if ends_up_resting {
            let node = OrderNode { id, account, side, kind, price, qty, remaining, seq, prev: None, next: None };
            self.rest(node);
        } else {
            if remaining > 0 {
                // Market/IOC/FOK with leftover: never rests, cancel it.
                // (Fok reaching here would itself be a bug -- the dry run
                // guaranteed full fillability -- but this still surfaces
                // as an honest event rather than a panic.)
                self.push_event(Event::Cancelled { id });
            }
            self.retirement.retire(id);
        }
    }

    /// Deliberately account-blind: post-only rejects on ANY cross,
    /// including a cross whose only counterparty is the same account's
    /// own resting order. This is not an oversight and there is no
    /// self-match carve-out to add. Rejection is the conservative choice
    /// for post-only specifically (it exists to guarantee an order never
    /// becomes a taker, full stop), unlike a resting Limit order crossed
    /// by an amend, which is allowed to take and has its self-match
    /// handled by CancelResting instead (see `handle_amend`). Those are
    /// two different kinds with two different, both intentional,
    /// self-match policies, not one inconsistent answer to one question.
    fn would_cross(&self, side: Side, price: Price) -> bool {
        match side {
            Side::Buy => self.asks.lowest_occupied().is_some_and(|t| (t as Price) <= price),
            Side::Sell => self.bids.highest_occupied().is_some_and(|t| (t as Price) >= price),
        }
    }

    /// FOK dry run. The engine cannot clone the book (preallocated arena,
    /// zero hot-path allocation), so this is a hand-rolled read-only
    /// counting pass: it walks the opposing side exactly like
    /// `run_match`, applying the identical `CancelResting` self-match
    /// skip (a same-account resting order contributes zero and is
    /// stepped over, never mutated), and sums what a real run would
    /// actually fill. Advances tick by tick via `lowest_occupied_after`/
    /// `highest_occupied_before` rather than re-querying "the current
    /// best," because nothing is ever removed here -- the level does not
    /// change out from under it the way it does in `run_match`.
    fn compute_fillable(&self, account: AccountId, side: Side, limit_price: Option<Price>, qty: Qty) -> Qty {
        let resting_side = side.opposite();
        let levels = match resting_side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        let mut remaining = qty;
        let mut tick_opt = match resting_side {
            Side::Buy => levels.highest_occupied(),
            Side::Sell => levels.lowest_occupied(),
        };

        while remaining > 0 {
            let Some(tick) = tick_opt else { break };
            let price = tick as Price;
            if let Some(lp) = limit_price {
                let crosses = match side {
                    Side::Buy => price <= lp,
                    Side::Sell => price >= lp,
                };
                if !crosses {
                    break;
                }
            }

            let mut cur = levels.head(tick);
            while let Some(h) = cur {
                let node = self.arena.get(h);
                if node.account != account {
                    let take = remaining.min(node.remaining);
                    remaining -= take;
                    if remaining == 0 {
                        break;
                    }
                }
                cur = node.next;
            }

            if remaining == 0 {
                break;
            }

            tick_opt = match resting_side {
                Side::Buy => levels.highest_occupied_before(tick),
                Side::Sell => levels.lowest_occupied_after(tick),
            };
        }

        qty - remaining
    }

    fn rest(&mut self, node: OrderNode) {
        let side = node.side;
        let tick = node.price as usize;
        let id = node.id;
        let handle = self.arena.alloc(node);
        self.live.insert(id, handle);
        match side {
            Side::Buy => push_back(&mut self.arena, &mut self.bids, tick, handle),
            Side::Sell => push_back(&mut self.arena, &mut self.asks, tick, handle),
        }
    }

    /// Mirrors `RefBook::run_match` exactly: re-derives the best level
    /// from the summary bitmap on every loop iteration instead of holding
    /// an iterator across mutation, which is what makes A3 (cancelling a
    /// resting order mid-walk via self-match) safe here -- there is
    /// nothing to invalidate, only a fresh bitmap scan each time.
    fn run_match(&mut self, taker_id: OrderId, taker_account: AccountId, side: Side, limit_price: Option<Price>, mut remaining: Qty) -> Qty {
        let resting_side = side.opposite();

        loop {
            if remaining == 0 {
                break;
            }
            let best_tick = match resting_side {
                Side::Buy => self.bids.highest_occupied(),
                Side::Sell => self.asks.lowest_occupied(),
            };
            let Some(tick) = best_tick else { break };
            let price = tick as Price;

            if let Some(lp) = limit_price {
                let crosses = match side {
                    Side::Buy => price <= lp,
                    Side::Sell => price >= lp,
                };
                if !crosses {
                    break;
                }
            }

            let front_handle = match resting_side {
                Side::Buy => self.bids.head(tick),
                Side::Sell => self.asks.head(tick),
            }
            .expect("occupied summary bit but empty level (internal corruption)");

            let front_account = self.arena.get(front_handle).account;

            if front_account == taker_account {
                // Self-match: CancelResting. Cancel outright, zero fill,
                // continue the walk at the same level.
                let id = self.arena.get(front_handle).id;
                match resting_side {
                    Side::Buy => pop_front(&mut self.arena, &mut self.bids, tick),
                    Side::Sell => pop_front(&mut self.arena, &mut self.asks, tick),
                };
                self.arena.free(front_handle);
                self.live.remove(id);
                self.retirement.retire(id);
                self.push_event(Event::Cancelled { id });
                continue;
            }

            let maker_qty = self.arena.get(front_handle).remaining;
            let fill_qty = remaining.min(maker_qty);
            let maker_id = self.arena.get(front_handle).id;

            self.arena.get_mut(front_handle).remaining -= fill_qty;

            // I3/I4 by construction: fills always happen at the maker's
            // resting price, and the crossing check above guarantees that
            // price is at or better than the taker's limit.
            debug_assert!(match side {
                Side::Buy => limit_price.is_none_or(|lp| price <= lp),
                Side::Sell => limit_price.is_none_or(|lp| price >= lp),
            });

            self.push_event(Event::Fill { maker: maker_id, taker: taker_id, price, qty: fill_qty });
            remaining -= fill_qty;

            let maker_done = self.arena.get(front_handle).remaining == 0;
            if maker_done {
                match resting_side {
                    Side::Buy => pop_front(&mut self.arena, &mut self.bids, tick),
                    Side::Sell => pop_front(&mut self.arena, &mut self.asks, tick),
                };
                self.arena.free(front_handle);
                self.live.remove(maker_id);
                self.retirement.retire(maker_id);
            }
        }

        remaining
    }

    // ---- Cancel -----------------------------------------------------------

    fn handle_cancel(&mut self, seq: Seq, id: OrderId) {
        let Some(&handle) = self.live.get(id) else {
            self.push_event(Event::Rejected { id, reason: RejectReason::UnknownOrder, seq });
            return;
        };
        let node = self.arena.get(handle);
        let side = node.side;
        let tick = node.price as usize;
        match side {
            Side::Buy => unlink(&mut self.arena, &mut self.bids, tick, handle),
            Side::Sell => unlink(&mut self.arena, &mut self.asks, tick, handle),
        }
        self.arena.free(handle);
        self.live.remove(id);
        self.retirement.retire(id);
        self.push_event(Event::Cancelled { id });
    }

    // ---- Amend --------------------------------------------------------------

    fn handle_amend(&mut self, seq: Seq, id: OrderId, new_price: Price, new_qty: Qty) {
        let Some(&handle) = self.live.get(id) else {
            self.push_event(Event::Rejected { id, reason: RejectReason::UnknownOrder, seq });
            return;
        };
        if new_qty == 0 {
            self.push_event(Event::Rejected { id, reason: RejectReason::InvalidQuantity, seq });
            return;
        }
        if !self.cfg.contains(new_price) {
            self.push_event(Event::Rejected { id, reason: RejectReason::PriceOutOfBand, seq });
            return;
        }

        let current = self.arena.get(handle).clone();
        let side = current.side;

        // Post-only never becomes a taker, even via amend. See
        // `reference::RefBook::handle_amend` for the full rationale:
        // `current.kind` only exists because kind is sticky on the
        // resting order, not the (kind-less) Amend command.
        if current.kind == Kind::PostOnly && self.would_cross(side, new_price) {
            self.push_event(Event::Rejected { id, reason: RejectReason::WouldCross, seq });
            return;
        }

        // new_qty is the new REMAINING quantity, compared against current
        // remaining, not the original total. See reference for why.
        let price_changed = new_price != current.price;
        let qty_increased = new_qty > current.remaining;
        let keeps_priority = !price_changed && !qty_increased;

        if keeps_priority {
            let node = self.arena.get_mut(handle);
            node.qty = new_qty;
            node.remaining = new_qty;
            self.push_event(Event::Amended { id, lost_priority: false });
            return;
        }

        // Quantity increase or any price change: unlink from the current
        // level but keep the same arena slot -- the id keeps its
        // identity, only its position and priority key change -- then
        // re-run matching exactly like a brand new Limit order. Amended
        // is emitted before any resulting Fill/Cancelled it causes, never
        // interleaved, matching reference.
        let old_tick = current.price as usize;
        match side {
            Side::Buy => unlink(&mut self.arena, &mut self.bids, old_tick, handle),
            Side::Sell => unlink(&mut self.arena, &mut self.asks, old_tick, handle),
        }
        self.live.remove(id);

        {
            let node = self.arena.get_mut(handle);
            node.price = new_price;
            node.qty = new_qty;
            node.remaining = new_qty;
            node.seq = seq;
            node.prev = None;
            node.next = None;
        }

        self.push_event(Event::Amended { id, lost_priority: true });

        let account = current.account;
        let remaining = self.run_match(id, account, side, Some(new_price), new_qty);

        if remaining > 0 {
            self.arena.get_mut(handle).remaining = remaining;
            let new_tick = new_price as usize;
            match side {
                Side::Buy => push_back(&mut self.arena, &mut self.bids, new_tick, handle),
                Side::Sell => push_back(&mut self.arena, &mut self.asks, new_tick, handle),
            }
            self.live.insert(id, handle);
        } else {
            // Fully consumed by its own re-match: the slot was never
            // re-linked into a level, so free it and retire the id, just
            // as a fully-filling New would.
            self.arena.free(handle);
            self.retirement.retire(id);
        }
    }
}

#[cfg(test)]
mod tests {
    //! CHECK 1 (Session 2 audit): the ported A2 and A8 attack-scenario
    //! tests both redden even with the arena's generation-mismatch
    //! assertion disabled -- but via the arena's *separate*
    //! slot-occupancy assertion, not via either test's own event/live-
    //! index assertions. Tracing both scenarios by hand explains why:
    //! in neither test does anything ever reuse the freed slot before
    //! the racing command arrives, so the slot is genuinely empty and
    //! the occupancy check catches it regardless of generation
    //! checking. That is not the ABA shape the generation counter
    //! exists for.
    //!
    //! The shape it exists for -- a *new*, different order actually
    //! reusing the freed slot before a stale reference to the old order
    //! surfaces -- cannot be constructed through the public `Command`
    //! API at all under correct (non-buggy) code, for exactly the
    //! reason F-002 already established for `reference`: `Cancel`/
    //! `Amend` resolve through `live`, which is correctly maintained,
    //! so no client command can ever present a stale handle. The only
    //! way to observe the generation counter actually doing its job is
    //! fault injection: hold a `Handle` from before a slot was freed and
    //! reused, bypassing `live` entirely, exactly as F-002 prescribes.
    //! That requires reaching into private fields, so it lives here as
    //! a crate-internal unit test rather than in `tests/`.
    use super::*;

    fn test_cfg() -> Config {
        Config { n_ticks: 256, capacity: 2, id_retirement_window: 8 }
    }

    #[test]
    #[should_panic(expected = "generation mismatch")]
    fn stale_handle_from_a_reused_slot_is_caught_by_generation_check() {
        let mut book = Book::new(test_cfg());

        book.apply(Command::New { id: 1, account: 1, side: Side::Buy, kind: Kind::Limit, price: 100, qty: 1 });
        let stale_handle = *book.live.get(1).expect("order 1 should be live and resting");

        // Cancel through the real, correct, unmutated code path: this
        // frees the slot and removes id 1 from `live`, exactly as a
        // genuine client Cancel would. `stale_handle` is now held only
        // by this test, bypassing `live` -- the shape no client command
        // can reproduce.
        book.apply(Command::Cancel { id: 1 });

        // `free` is a LIFO stack (`Vec::pop`), so whichever slot was
        // freed most recently is handed out first. Order 1's slot was
        // the only one ever freed, so this New deterministically reuses
        // it, regardless of the arena's total capacity.
        book.apply(Command::New { id: 2, account: 2, side: Side::Sell, kind: Kind::Limit, price: 100, qty: 1 });

        // `stale_handle` shares its index with order 2's real handle
        // but carries order 1's old, now-superseded generation. This is
        // the exact shape the generation counter exists to catch:
        // internal corruption unreachable through the public API.
        let _ = book.arena.get(stale_handle);
    }
}
