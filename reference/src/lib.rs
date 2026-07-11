//! `reference`: the naive, obviously-correct oracle book.
//!
//! Rules enforced socially, per the architecture doc:
//! - No performance change is ever made here, no matter how obvious.
//! - If a line is not immediately readable as the definition of correct,
//!   it is rewritten until it is.
//! - Any disagreement between this and the optimized engine is resolved in
//!   favor of this crate until proven otherwise.
//!
//! This crate is the specification. `engine` is an implementation of it.

#![forbid(unsafe_code)]

mod invariants;

pub use invariants::check_invariants;

use std::cmp::Reverse;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::hash::{Hash, Hasher};

use types::{AccountId, Command, Config, Event, Kind, OrderId, Price, Qty, RejectReason, RetirementRing, Seq, Side};

/// `price * quantity`, widened to `i128` before multiplication per
/// architecture section 2. Nothing in the current order-matching path
/// needs a notional figure (there is no fee, margin, or risk-limit logic
/// in scope), so this exists purely as the widening utility A6 requires
/// to be tested explicitly and not "discovered" implicitly by a bug.
pub fn notional(price: Price, qty: Qty) -> i128 {
    (price as i128) * (qty as i128)
}

/// The reference engine's own order representation. Deliberately dumb: a
/// plain struct in a `VecDeque`, no arena, no intrusive links.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Order {
    pub id: OrderId,
    pub account: AccountId,
    pub side: Side,
    pub kind: Kind,
    pub price: Price,
    pub qty: Qty,
    pub remaining: Qty,
    pub seq: Seq,
}

/// `#[derive(Clone)]` here is deliberately total: it clones every field,
/// including `live` and `next_seq`, not just `bids`/`asks`. A partial
/// clone -- forgetting `live`, say -- would give `compute_fillable`'s
/// shadow book a different account/liveness picture than the real book,
/// which is A4 reappearing inside the exact mechanism built to prevent
/// A4. See `clone_is_total_and_independent` in the test suite.
#[derive(Debug, Clone)]
pub struct RefBook {
    cfg: Config,
    pub(crate) bids: BTreeMap<Reverse<Price>, VecDeque<Order>>,
    pub(crate) asks: BTreeMap<Price, VecDeque<Order>>,
    pub(crate) live: HashMap<OrderId, Side>,
    retirement: RetirementRing,
    next_seq: Seq,
    log: Vec<Command>,
}

impl RefBook {
    pub fn new(cfg: Config) -> Self {
        RefBook {
            cfg,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            live: HashMap::new(),
            retirement: RetirementRing::new(cfg.id_retirement_window),
            next_seq: 0,
            log: Vec::new(),
        }
    }

    /// Simple `live.len() >= capacity` check: the reference engine's index
    /// holds only resting orders, so this is the entire arena-capacity
    /// story here. `engine` mirrors this with its own free-slot count.
    fn arena_full(&self) -> bool {
        self.live.len() >= self.cfg.capacity
    }

    pub fn config(&self) -> Config {
        self.cfg
    }

    pub fn best_bid(&self) -> Option<Price> {
        self.bids.keys().next().map(|Reverse(p)| *p)
    }

    pub fn best_ask(&self) -> Option<Price> {
        self.asks.keys().next().copied()
    }

    pub fn is_live(&self, id: OrderId) -> bool {
        self.live.contains_key(&id)
    }

    pub fn live_count(&self) -> usize {
        self.live.len()
    }

    pub fn command_log(&self) -> &[Command] {
        &self.log
    }

    /// A stable hash of the full book, walked in canonical priority order
    /// (best-to-worst price, FIFO within a level). Used by the I9 replay
    /// check. Deliberately does not touch `next_seq` or `log` directly,
    /// only the resting orders, since replay only needs to prove the
    /// *book* state is byte-identical.
    pub fn digest(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        "bids".hash(&mut hasher);
        for (Reverse(price), level) in self.bids.iter() {
            price.hash(&mut hasher);
            for o in level {
                o.hash(&mut hasher);
            }
        }
        "asks".hash(&mut hasher);
        for (price, level) in self.asks.iter() {
            price.hash(&mut hasher);
            for o in level {
                o.hash(&mut hasher);
            }
        }
        hasher.finish()
    }

    /// I9: replay the command log into a fresh engine and compare digests.
    pub fn replay_matches(&self) -> bool {
        let mut fresh = RefBook::new(self.cfg);
        for cmd in &self.log {
            fresh.apply(*cmd);
        }
        fresh.digest() == self.digest()
    }

    /// Sequence is assigned here, at ingress, before matching, before the
    /// accept/reject decision even exists. One counter, incremented on
    /// every command -- New, Cancel, or Amend -- whether it ultimately
    /// gets accepted or rejected. This is what makes A2 statable at all:
    /// "a cancel arriving one seq after the fill" requires the cancel to
    /// occupy a sequence number even when it is about to be rejected as
    /// UnknownOrder.
    pub fn apply(&mut self, cmd: Command) -> Vec<Event> {
        self.log.push(cmd);
        let seq = self.next_seq;
        self.next_seq += 1;
        match cmd {
            Command::New { id, account, side, kind, price, qty } => {
                self.handle_new(seq, id, account, side, kind, price, qty)
            }
            Command::Cancel { id } => self.handle_cancel(seq, id),
            Command::Amend { id, new_price, new_qty } => self.handle_amend(seq, id, new_price, new_qty),
        }
    }

    // ---- New ----------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn handle_new(
        &mut self,
        seq: Seq,
        id: OrderId,
        account: AccountId,
        side: Side,
        kind: Kind,
        price: Price,
        qty: Qty,
    ) -> Vec<Event> {
        let mut events = Vec::new();

        if qty == 0 {
            events.push(Event::Rejected { id, reason: RejectReason::InvalidQuantity, seq });
            return events;
        }
        if self.live.contains_key(&id) {
            events.push(Event::Rejected { id, reason: RejectReason::DuplicateId, seq });
            return events;
        }
        // Not live right now, but retired too recently: the ABA guard
        // from F-001. Distinct from DuplicateId (checked above) because
        // the failure shape is different -- this id *was* live and is
        // gone, not currently in use.
        if self.retirement.contains(id) {
            events.push(Event::Rejected { id, reason: RejectReason::DuplicateOrderId, seq });
            return events;
        }
        // Market orders carry no meaningful price; the band does not apply.
        if kind != Kind::Market && !self.cfg.contains(price) {
            events.push(Event::Rejected { id, reason: RejectReason::PriceOutOfBand, seq });
            return events;
        }
        // Checked at ingress, before matching, before any mutation: a
        // marketable order that would have fully filled without ever
        // resting is still rejected here if the arena is full. This is
        // the only point where "mutate nothing" is still achievable --
        // once fills have been emitted there is no honest event left to
        // send for a reject. See Config::capacity.
        if self.arena_full() {
            events.push(Event::Rejected { id, reason: RejectReason::ArenaFull, seq });
            return events;
        }

        let limit_price = match kind {
            Kind::Market => None,
            _ => Some(price),
        };

        if kind == Kind::PostOnly && self.would_cross(side, price) {
            events.push(Event::Rejected { id, reason: RejectReason::WouldCross, seq });
            return events;
        }

        if kind == Kind::Fok {
            let fillable = self.compute_fillable(account, side, limit_price, qty);
            if fillable < qty {
                events.push(Event::Rejected { id, reason: RejectReason::Unfillable, seq });
                return events;
            }
        }

        events.push(Event::Accepted { id, seq });

        let mut order = Order { id, account, side, kind, price, qty, remaining: qty, seq };
        let remaining = self.run_match(order.id, order.account, order.side, limit_price, order.remaining, &mut events);
        order.remaining = remaining;

        // An order that ends up resting stays live; retirement does not
        // apply to it yet. Every other path here used the id without it
        // becoming a durable resting order, so the id retires now: a late
        // Cancel/Amend/New naming it must not silently land on whatever
        // New later reuses the id.
        let ends_up_resting = remaining > 0 && matches!(kind, Kind::Limit | Kind::PostOnly);

        if remaining > 0 {
            match kind {
                Kind::Limit | Kind::PostOnly => self.rest(order),
                Kind::Market | Kind::Ioc | Kind::Fok => {
                    // Fok reaching here with remaining > 0 would itself be a
                    // reference-engine bug (the dry run guaranteed full
                    // fillability). We do not paper over it: it still
                    // surfaces as a Cancelled event rather than a panic, so
                    // a test can catch the divergence from the promised
                    // semantics.
                    events.push(Event::Cancelled { id });
                }
            }
        }

        if !ends_up_resting {
            self.retirement.retire(id);
        }

        events
    }

    fn would_cross(&self, side: Side, price: Price) -> bool {
        match side {
            Side::Buy => self.best_ask().is_some_and(|ask| ask <= price),
            Side::Sell => self.best_bid().is_some_and(|bid| bid >= price),
        }
    }

    /// FOK dry run. Runs the *exact same* matching code as real execution,
    /// against a full clone of the book, and throws the clone away. This
    /// is deliberate: sharing the code path is the only way to guarantee
    /// the dry run applies the identical self-match policy as the real
    /// run (A4). A hand-rolled "count what's available" function would be
    /// a second implementation of matching that could drift from the
    /// first.
    ///
    /// Decision, not a side effect: a FOK that rejects also rolls back
    /// any self-match cancellations the dry run performed while walking
    /// the clone, because the clone -- and everything it cancelled -- is
    /// discarded. This is consistent with "mutate nothing" (see the FOK
    /// semantics in the architecture doc) and is load-bearing: without
    /// it, a same-account resting order sitting in front of otherwise
    /// sufficient liquidity would get permanently cancelled by a FOK that
    /// never actually executed.
    fn compute_fillable(&self, account: AccountId, side: Side, limit_price: Option<Price>, qty: Qty) -> Qty {
        let mut shadow = self.clone();
        let mut events = Vec::new();
        let remaining_after = shadow.run_match(OrderId::MAX, account, side, limit_price, qty, &mut events);
        qty - remaining_after
    }

    fn rest(&mut self, order: Order) {
        self.live.insert(order.id, order.side);
        match order.side {
            Side::Buy => self.bids.entry(Reverse(order.price)).or_default().push_back(order),
            Side::Sell => self.asks.entry(order.price).or_default().push_back(order),
        }
    }

    /// Walks the opposing book from the best price outward, matching
    /// `remaining` quantity of a taker at `taker_id`/`taker_account` on
    /// `side`, subject to `limit_price` (`None` = market, sweep at any
    /// price). Applies `CancelResting` self-match prevention inline.
    ///
    /// The best level is re-queried from the map on every loop iteration
    /// instead of holding an iterator across mutation. This is what makes
    /// A3's "cancel the resting order mid-walk" case safe here: there is
    /// no iterator to invalidate, only a fresh lookup each time.
    fn run_match(
        &mut self,
        taker_id: OrderId,
        taker_account: AccountId,
        side: Side,
        limit_price: Option<Price>,
        mut remaining: Qty,
        events: &mut Vec<Event>,
    ) -> Qty {
        let resting_side = side.opposite();

        loop {
            if remaining == 0 {
                break;
            }
            let best_price = match resting_side {
                Side::Buy => self.bids.keys().next().map(|Reverse(p)| *p),
                Side::Sell => self.asks.keys().next().copied(),
            };
            let Some(price) = best_price else { break };

            if let Some(lp) = limit_price {
                let crosses = match side {
                    Side::Buy => price <= lp,
                    Side::Sell => price >= lp,
                };
                if !crosses {
                    break;
                }
            }

            let front_account = self.level_mut(resting_side, price).front().unwrap().account;

            if front_account == taker_account {
                // Self-match: CancelResting. The resting order is
                // cancelled outright, contributes zero fill, and the walk
                // continues at the same level/price.
                let cancelled = self.level_mut(resting_side, price).pop_front().unwrap();
                self.live.remove(&cancelled.id);
                self.retirement.retire(cancelled.id);
                self.drop_level_if_empty(resting_side, price);
                events.push(Event::Cancelled { id: cancelled.id });
                continue;
            }

            let maker_qty = self.level_mut(resting_side, price).front().unwrap().remaining;
            let fill_qty = remaining.min(maker_qty);

            let maker_id = {
                let front = self.level_mut(resting_side, price).front_mut().unwrap();
                front.remaining -= fill_qty;
                front.id
            };

            // I3/I4 by construction: fills always happen at the maker's
            // resting price, and the crossing check above guarantees that
            // price is at or better than the taker's limit.
            debug_assert!(match side {
                Side::Buy => limit_price.is_none_or(|lp| price <= lp),
                Side::Sell => limit_price.is_none_or(|lp| price >= lp),
            });

            events.push(Event::Fill { maker: maker_id, taker: taker_id, price, qty: fill_qty });
            remaining -= fill_qty;

            let maker_done = self.level_mut(resting_side, price).front().unwrap().remaining == 0;
            if maker_done {
                let done = self.level_mut(resting_side, price).pop_front().unwrap();
                self.live.remove(&done.id);
                self.retirement.retire(done.id);
            }
            self.drop_level_if_empty(resting_side, price);
        }

        remaining
    }

    fn level_mut(&mut self, side: Side, price: Price) -> &mut VecDeque<Order> {
        match side {
            Side::Buy => self.bids.get_mut(&Reverse(price)).expect("level must exist"),
            Side::Sell => self.asks.get_mut(&price).expect("level must exist"),
        }
    }

    fn drop_level_if_empty(&mut self, side: Side, price: Price) {
        match side {
            Side::Buy => {
                if self.bids.get(&Reverse(price)).is_some_and(|d| d.is_empty()) {
                    self.bids.remove(&Reverse(price));
                }
            }
            Side::Sell => {
                if self.asks.get(&price).is_some_and(|d| d.is_empty()) {
                    self.asks.remove(&price);
                }
            }
        }
    }

    // ---- Cancel ---------------------------------------------------------

    fn handle_cancel(&mut self, seq: Seq, id: OrderId) -> Vec<Event> {
        let mut events = Vec::new();
        let Some(&side) = self.live.get(&id) else {
            events.push(Event::Rejected { id, reason: RejectReason::UnknownOrder, seq });
            return events;
        };
        self.remove_order(side, id);
        self.live.remove(&id);
        self.retirement.retire(id);
        events.push(Event::Cancelled { id });
        events
    }

    // ---- Amend ------------------------------------------------------------

    fn handle_amend(&mut self, seq: Seq, id: OrderId, new_price: Price, new_qty: Qty) -> Vec<Event> {
        let mut events = Vec::new();

        let Some(&side) = self.live.get(&id) else {
            events.push(Event::Rejected { id, reason: RejectReason::UnknownOrder, seq });
            return events;
        };
        if new_qty == 0 {
            events.push(Event::Rejected { id, reason: RejectReason::InvalidQuantity, seq });
            return events;
        }
        if !self.cfg.contains(new_price) {
            events.push(Event::Rejected { id, reason: RejectReason::PriceOutOfBand, seq });
            return events;
        }

        let current = self.peek(side, id).clone();

        // Post-only never becomes a taker, even via amend: a repriced
        // post-only that would now cross is rejected outright and the
        // resting order is left untouched. See README/summary for why
        // this is an assumption, not spec text. `current.kind` only
        // exists because Kind is sticky on the resting Order -- Command
        // carries no kind for Amend, so without recording it on the
        // order itself there would be no way to tell a Limit-turned-
        // crossing (matches) from a PostOnly-turned-crossing (rejects)
        // apart.
        if current.kind == Kind::PostOnly && self.would_cross(side, new_price) {
            events.push(Event::Rejected { id, reason: RejectReason::WouldCross, seq });
            return events;
        }

        // new_qty is the new REMAINING quantity, not the new total. An
        // order that filled 40 of 100 and is amended to new_qty=50 ends
        // up with 50 remaining (not 10), compared against its current 60
        // remaining to decide the priority rule: 50 < 60 is a decrease,
        // keeps priority. This also makes amending below the
        // already-filled amount inexpressible by construction -- there
        // is no "total" to under-shoot.
        let price_changed = new_price != current.price;
        let qty_increased = new_qty > current.remaining;
        let keeps_priority = !price_changed && !qty_increased;

        if keeps_priority {
            // Quantity decrease only, same price: mutate in place, no
            // repositioning, no rematching. A resting order that did not
            // cross before cannot start crossing purely by shrinking, so
            // there is nothing to walk here.
            self.mutate_in_place(side, id, |o| {
                o.qty = new_qty;
                o.remaining = new_qty;
            });
            events.push(Event::Amended { id, lost_priority: false });
            return events;
        }

        // Quantity increase or any price change: remove, re-price, assign
        // the amend's own ingress seq as the new priority key, then
        // re-insert exactly like a brand new Limit order, which means it
        // matches against the book if it now crosses (this is what A3
        // requires). Amended is emitted before any resulting Fill events,
        // never interleaved: that ordering is pinned as spec, since the
        // differential fuzzer compares event vectors elementwise.
        let mut removed = self.remove_order(side, id).expect("live index says this order exists");
        self.live.remove(&id);

        removed.price = new_price;
        removed.qty = new_qty;
        removed.remaining = new_qty;
        removed.seq = seq;

        events.push(Event::Amended { id, lost_priority: true });

        let account = removed.account;
        let remaining = self.run_match(id, account, side, Some(new_price), removed.remaining, &mut events);
        removed.remaining = remaining;

        if remaining > 0 {
            self.rest(removed);
        } else {
            // Fully consumed by its own re-match: the id is no longer
            // live and was never re-inserted, so it retires here just as
            // it would from handle_new's non-resting path.
            self.retirement.retire(id);
        }

        events
    }

    fn peek(&self, side: Side, id: OrderId) -> &Order {
        match side {
            Side::Buy => self.bids.values().flat_map(|d| d.iter()).find(|o| o.id == id),
            Side::Sell => self.asks.values().flat_map(|d| d.iter()).find(|o| o.id == id),
        }
        .expect("live index consistent with book contents")
    }

    fn mutate_in_place(&mut self, side: Side, id: OrderId, f: impl FnOnce(&mut Order)) {
        let found = match side {
            Side::Buy => self.bids.values_mut().flat_map(|d| d.iter_mut()).find(|o| o.id == id),
            Side::Sell => self.asks.values_mut().flat_map(|d| d.iter_mut()).find(|o| o.id == id),
        };
        f(found.expect("live index consistent with book contents"));
    }

    /// Linear scan of every level on `side` for `id`; removes it and drops
    /// the level if it becomes empty. Returns the removed order, if any.
    fn remove_order(&mut self, side: Side, id: OrderId) -> Option<Order> {
        let removed = match side {
            Side::Buy => Self::remove_from_map(&mut self.bids, id),
            Side::Sell => Self::remove_from_map(&mut self.asks, id),
        };
        self.bids.retain(|_, d| !d.is_empty());
        self.asks.retain(|_, d| !d.is_empty());
        removed
    }

    fn remove_from_map<K: Ord + Copy>(map: &mut BTreeMap<K, VecDeque<Order>>, id: OrderId) -> Option<Order> {
        for level in map.values_mut() {
            if let Some(pos) = level.iter().position(|o| o.id == id) {
                return level.remove(pos);
            }
        }
        None
    }
}
