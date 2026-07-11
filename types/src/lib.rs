//! Shared `Command` / `Event` vocabulary for `Shadowbook`.
//!
//! Both the `reference` (oracle) engine and the `engine` (optimized) engine
//! speak this protocol. Neither crate is allowed to define its own
//! divergent notion of what a command or an event is, which would make the
//! differential fuzzer meaningless.

#![forbid(unsafe_code)]

use std::collections::{HashMap, VecDeque};

/// Client-supplied, uniqueness enforced by the engine.
pub type OrderId = u64;
/// Only exists to drive self-match prevention (`CancelResting`).
pub type AccountId = u32;
/// Ticks from a fixed reference. Never a float.
pub type Price = i64;
/// Lots. Never a float.
pub type Qty = u64;
/// Assigned by the engine at ingress, before matching, before the
/// accept/reject decision exists. One counter, incremented on every
/// command including rejected ones. Doubles as the time priority key for
/// resting orders: unique by construction, so ties within a level are
/// structurally impossible.
pub type Seq = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn opposite(self) -> Side {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Limit,
    Market,
    Ioc,
    Fok,
    PostOnly,
}

/// Everything a book needs to be constructed identically by both engines.
/// Every field here is enforced identically by `reference` and `engine`,
/// because the differential fuzzer feeds both engines the same commands
/// under the same `Config` and any divergence in what a `Config` means
/// would show up as a false-positive divergence rather than a real bug.
///
/// `n_ticks` is a runtime field, not a compile-time constant, so tests and
/// fuzzing can run against a small, cheaply-exhaustible band (see
/// `DEFAULT_CONFIG`) while benchmarks run against a production-sized one
/// (see `BENCH_CONFIG`). Ticks are absolute: `TickIdx = tick as usize`, no
/// offset arithmetic, band `[0, n_ticks - 1]`. The band is anchored at
/// zero so the sharp edge (a `usize` underflow near the low boundary)
/// stays directly reachable instead of being hidden behind an offset
/// subtraction that would be correct by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub n_ticks: usize,
    /// Maximum number of simultaneously resting orders. Enforced
    /// identically in both engines at New ingress, before matching, before
    /// any mutation: see `RejectReason::ArenaFull`.
    pub capacity: usize,
    /// Size of the retired-order-id ring. Enforced identically in both
    /// engines at New ingress: see `RejectReason::DuplicateOrderId`.
    pub id_retirement_window: usize,
}

impl Config {
    pub fn tick_max(&self) -> Price {
        self.n_ticks as Price - 1
    }

    pub fn contains(&self, price: Price) -> bool {
        price >= 0 && price < self.n_ticks as Price
    }
}

/// Small band, small arena, small retirement window: cheap enough for unit
/// tests and for the fuzzer to exhaust the interesting states (band edges,
/// arena exhaustion, id reuse) in a short run. `id_retirement_window` is
/// `capacity * 4` so fuzz-scale churn collides with it constantly.
pub const DEFAULT_CONFIG: Config = Config { n_ticks: 256, capacity: 64, id_retirement_window: 256 };

/// Production-sized band and arena, for `BENCH.md` latency measurement
/// only. Not used by tests or the fuzzer; a run at this scale is a
/// different, much slower, instrument.
pub const BENCH_CONFIG: Config = Config { n_ticks: 65536, capacity: 65536, id_retirement_window: 262_144 };

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RejectReason {
    UnknownOrder,
    DuplicateId,
    /// The id is not currently live, but was live or used within the last
    /// `Config::id_retirement_window` retirements. Distinct from
    /// `DuplicateId` (id is live right now): this is the ABA guard from
    /// F-001, id reuse across a fill-then-New or cancel-then-New gap.
    DuplicateOrderId,
    InvalidQuantity,
    /// Outside `[0, Config::n_ticks - 1]`.
    PriceOutOfBand,
    WouldCross,
    Unfillable,
    /// No free slot at ingress. Checked before matching, before any
    /// mutation, so a marketable order that would have fully filled
    /// without ever resting is still rejected if the arena is full: see
    /// the architecture note on `Config::capacity` for why this
    /// conservative rule is the only one that stays mirrorable and mutates
    /// nothing on reject.
    ArenaFull,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    New {
        id: OrderId,
        account: AccountId,
        side: Side,
        kind: Kind,
        price: Price,
        qty: Qty,
    },
    Cancel {
        id: OrderId,
    },
    Amend {
        id: OrderId,
        new_price: Price,
        new_qty: Qty,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Accepted { id: OrderId, seq: Seq },
    /// Carries `seq` because rejects consume ingress sequence space too:
    /// A2 (a cancel racing a fill) is defined as "the cancel arrives one
    /// seq after the fill." If a rejected command consumed no sequence
    /// number, a rejected cancel would be invisible in sequence space and
    /// that scenario could not even be stated, let alone tested.
    Rejected { id: OrderId, reason: RejectReason, seq: Seq },
    Fill { maker: OrderId, taker: OrderId, price: Price, qty: Qty },
    Cancelled { id: OrderId },
    Amended { id: OrderId, lost_priority: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OrderContext {
    side: Side,
    kind: Kind,
    /// Current resting limit price. Meaningless for `Kind::Market`
    /// (never checked for it).
    price: Price,
    /// Remaining quantity as the auditor understands it: set to the
    /// command's `qty` on Accept, overwritten to `new_qty` on Amend
    /// (Amend's `new_qty` is itself the new remaining, not a delta), and
    /// debited by each Fill this order participates in, maker or taker.
    /// This is what makes overfill detectable independently of either
    /// engine's own bookkeeping: the auditor keeps its own count, fed
    /// only by emitted events.
    remaining: Qty,
}

/// Running-total auditor over an event stream. I2 (quantity
/// conservation), I3 (no fill violates its limit), and I4 (fills occur at
/// the maker's price) are not properties of a single book snapshot; they
/// only exist across a sequence of commands and their events, so they
/// cannot be implemented as `check_invariants(&book)` the way I1/I5/I6/I7
/// are. This type lives in `types` so both `reference` and `engine` audit
/// against one shared implementation rather than two that can drift.
///
/// Usage: call `observe(cmd, events)` once per `apply()` call, in order.
/// Then check `violations` (should stay empty) and `quantity_conserved()`.
///
/// `orders` never evicts an entry: nothing here bounds how many distinct
/// ids it can accumulate over a run. It stays small in practice only
/// because the fuzz generator's own id pool is bounded and ids cycle
/// through it; that bound is external to this type; it is not enforced
/// here.
#[derive(Debug, Default)]
pub struct EventAuditor {
    orders: HashMap<OrderId, OrderContext>,
    pub buy_filled: u128,
    pub sell_filled: u128,
    pub violations: Vec<String>,
}

impl EventAuditor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn observe(&mut self, cmd: &Command, events: &[Event]) {
        match cmd {
            Command::New { id, side, kind, price, qty, .. } => {
                if events.iter().any(|e| matches!(e, Event::Accepted { id: eid, .. } if eid == id)) {
                    self.orders.insert(*id, OrderContext { side: *side, kind: *kind, price: *price, remaining: *qty });
                }
            }
            Command::Amend { id, new_price, new_qty } => {
                if events.iter().any(|e| matches!(e, Event::Amended { id: eid, .. } if eid == id)) {
                    if let Some(ctx) = self.orders.get_mut(id) {
                        ctx.price = *new_price;
                        ctx.remaining = *new_qty;
                    }
                }
            }
            Command::Cancel { .. } => {}
        }

        // I5, the property the name actually promises: within one command's
        // resulting fills, price moves monotonically WORSE for the taker as
        // the sweep continues, never back to something better. A buy sweep
        // fills its best (lowest) available level first, then progressively
        // higher levels as the best is exhausted, so consecutive fill
        // prices are expected to be non-decreasing; a later fill at a LOWER
        // price than an earlier one means a better level's liquidity was
        // left unfilled while a worse level was matched first, a genuine
        // price-priority violation. Symmetric for a sell sweep: prices are
        // expected non-increasing, and a later, HIGHER price is the
        // violation. This is a taker-side, cross-fill property that neither
        // `check_fill` (which checks one fill against its own maker/taker
        // limits in isolation) nor either engine's own bitmap/occupancy
        // check covers.
        let mut last_fill: Option<(Side, Price)> = None;
        for e in events {
            if let Event::Fill { maker, taker, price, qty } = e {
                self.check_fill(*maker, *taker, *price, *qty);
                self.check_fill_exhaustion_order(*taker, *price, &mut last_fill);
            }
        }
    }

    fn check_fill_exhaustion_order(&mut self, taker: OrderId, price: Price, last_fill: &mut Option<(Side, Price)>) {
        let Some(side) = self.orders.get(&taker).map(|ctx| ctx.side) else {
            return; // unknown taker is already flagged by check_fill
        };
        if let Some((prev_side, prev_price)) = *last_fill {
            if prev_side == side {
                let worsened_then_improved = match side {
                    Side::Buy => price < prev_price,
                    Side::Sell => price > prev_price,
                };
                if worsened_then_improved {
                    self.violations.push(format!(
                        "I5 (fill exhaustion order): taker {taker} ({side:?}) filled at {price} after already filling at {prev_price} in the same command, a better level was left unfilled while a worse one matched first"
                    ));
                }
            }
        }
        *last_fill = Some((side, price));
    }

    fn check_fill(&mut self, maker: OrderId, taker: OrderId, price: Price, qty: Qty) {
        match self.orders.get(&maker).copied() {
            Some(ctx) => {
                // I4: fills occur at the maker's price, not some
                // improved or midpoint price.
                if ctx.kind != Kind::Market && ctx.price != price {
                    self.violations.push(format!(
                        "I4: fill at {price} does not match maker {maker}'s resting price {}",
                        ctx.price
                    ));
                }
                self.check_limit(maker, ctx, price, "I3 (maker)");
                self.check_and_debit_remaining(maker, qty, "maker");
                self.credit(ctx.side, qty);
            }
            None => self.violations.push(format!("fill references unknown maker {maker}")),
        }

        match self.orders.get(&taker).copied() {
            Some(ctx) => {
                self.check_limit(taker, ctx, price, "I3 (taker)");
                self.check_and_debit_remaining(taker, qty, "taker");
                self.credit(ctx.side, qty);
            }
            None => self.violations.push(format!("fill references unknown taker {taker}")),
        }
    }

    /// Overfill: a fill must never exceed the order's own remaining
    /// quantity, as the auditor has tracked it purely from prior emitted
    /// events. Covers both an ordinary overfill and a fill against an
    /// order that already has zero remaining (the same condition, since
    /// a `Fill`'s `qty` is always positive): `qty > 0 == qty > remaining`
    /// when `remaining` is already `0`. Debits on success only, so one
    /// bad fill does not cascade into spurious follow-on violations from
    /// an already-corrupted running total.
    fn check_and_debit_remaining(&mut self, id: OrderId, qty: Qty, tag: &str) {
        if let Some(ctx) = self.orders.get_mut(&id) {
            if qty > ctx.remaining {
                self.violations.push(format!("overfill ({tag}): order {id} filled {qty} against {} remaining", ctx.remaining));
            } else {
                ctx.remaining -= qty;
            }
        }
    }

    /// I3: the promise the word "limit" makes. No buy order fills above
    /// its limit; no sell order fills below it. Market orders have no
    /// limit to violate.
    fn check_limit(&mut self, id: OrderId, ctx: OrderContext, fill_price: Price, tag: &str) {
        if ctx.kind == Kind::Market {
            return;
        }
        let ok = match ctx.side {
            Side::Buy => fill_price <= ctx.price,
            Side::Sell => fill_price >= ctx.price,
        };
        if !ok {
            self.violations.push(format!(
                "{tag}: order {id} ({:?} limit {}) filled at {fill_price}",
                ctx.side, ctx.price
            ));
        }
    }

    /// Credited independently from each side's *recorded* identity (set
    /// at Accept/Amend time from the original command), not inferred
    /// from the Fill event itself. If a bug ever let a maker and taker
    /// land on the same side, this would show up as an imbalance instead
    /// of silently doubling a number that was never independently
    /// cross-checked.
    fn credit(&mut self, side: Side, qty: Qty) {
        match side {
            Side::Buy => self.buy_filled += qty as u128,
            Side::Sell => self.sell_filled += qty as u128,
        }
    }

    /// I2: sum of buy-side filled quantity equals sum of sell-side filled
    /// quantity, over the whole run.
    pub fn quantity_conserved(&self) -> bool {
        self.buy_filled == self.sell_filled
    }
}

/// Bounded FIFO ring of recently-retired order ids, shared by both
/// engines so `RejectReason::DuplicateOrderId` fires under identical
/// conditions in each. An id is retired the moment it stops being live,
/// whether because it filled to completion, was cancelled, was cancelled
/// via self-match `CancelResting`, or never rested at all (a Market/IOC/
/// FOK/fully-crossing order that used its id without ever becoming a
/// resting order). Membership answers "is this id too recently retired
/// to reuse."
///
/// `members` is reserved up front and never grows past that reservation:
/// an insert is always preceded by an eviction once the ring is at
/// capacity, so this structure never allocates once constructed. That is
/// what the hot path (`engine`'s ingress check) depends on -- and it is
/// why `members` is a hand-rolled `FixedIdMap`, not `std::collections::
/// HashSet`. A `HashSet` bounded the same way (evict-before-insert, never
/// exceeding `window` live entries) still grows internally under
/// sustained insert/remove churn: hashbrown's tombstone accounting
/// eventually forces a table resize even though occupancy never grows,
/// which is exactly the kind of thing `engine`'s zero-allocation test
/// exists to catch, and did (twice: once here, and once more in
/// `engine::Book`'s own live index, found later by a churn pattern this
/// test had not originally exercised -- see `FixedIdMap`'s own doc
/// comment). `FixedIdMap` uses backward-shift deletion (no tombstones),
/// so there is nothing to accumulate and no code path that could ever
/// reallocate it.
#[derive(Debug, Clone)]
pub struct RetirementRing {
    window: usize,
    order: VecDeque<OrderId>,
    members: FixedIdMap<()>,
}

impl RetirementRing {
    pub fn new(window: usize) -> Self {
        RetirementRing { window, order: VecDeque::with_capacity(window), members: FixedIdMap::with_capacity(window) }
    }

    pub fn contains(&self, id: OrderId) -> bool {
        self.members.contains_key(id)
    }

    /// Currently-retired ids, oldest first. For the fuzz generator, which
    /// needs to pick a random retired id to deliberately target the
    /// reused-id (`DuplicateOrderId`) scenario.
    pub fn iter(&self) -> impl Iterator<Item = OrderId> + '_ {
        self.order.iter().copied()
    }

    /// Retire `id`, evicting the oldest entry first if the ring is
    /// already at `window` capacity. A no-op if `window == 0` (retirement
    /// disabled) or if `id` is already present (should not happen given
    /// the uniqueness checks upstream, but kept safe rather than panicking
    /// on a duplicate retirement).
    pub fn retire(&mut self, id: OrderId) {
        if self.window == 0 {
            return;
        }
        if self.members.contains_key(id) {
            return;
        }
        if self.order.len() >= self.window {
            if let Some(evicted) = self.order.pop_front() {
                self.members.remove(evicted);
            }
        }
        self.order.push_back(id);
        self.members.insert(id, ());
    }
}

/// Fixed-capacity open-addressing map keyed by `OrderId`, linear probing
/// with backward-shift deletion (the Robin-Hood-hashing removal
/// algorithm): removing an entry immediately slides later-probed entries
/// backward into the freed slot rather than leaving a tombstone behind,
/// so there is nothing for repeated insert/remove cycles to accumulate
/// and no growth heuristic to ever trigger. Capacity is fixed at
/// construction (`slots.len()` is a power of two, sized for a max ~50%
/// load factor) and never changes: there is no resize code path in this
/// type at all, which is the actual guarantee, not a probabilistic
/// improvement from reserving extra headroom in a structure that can
/// still resize.
///
/// This is the general, value-carrying form of what was first written as
/// a set-only `FixedIdSet` for `RetirementRing`. A second, independent
/// instance of the class of bug this type exists to close was later
/// found in `engine::Book`'s `live: HashMap<OrderId, Handle>` index by a
/// zero-allocation test extended to hold occupancy near capacity under
/// sustained churn, a shape the original test never exercised (it always
/// drained the book to zero between commands). `live` is fixed the same
/// way, as `FixedIdMap<Handle>`, reusing this exact implementation
/// rather than a second copy of the same probing and deletion logic.
/// Bounded occupancy of a `std` hash collection reserved once at
/// construction is not the same guarantee as a table that cannot resize;
/// every such collection reachable from a sustained command stream is a
/// candidate for this same fix, not just the two found so far.
#[derive(Debug, Clone)]
pub struct FixedIdMap<V> {
    slots: Vec<Option<(OrderId, V)>>,
    mask: usize,
    len: usize,
}

impl<V> FixedIdMap<V> {
    pub fn with_capacity(min_capacity: usize) -> Self {
        let cap = (min_capacity.max(1) * 2).next_power_of_two();
        let slots = (0..cap).map(|_| None).collect();
        FixedIdMap { slots, mask: cap - 1, len: 0 }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn slot_for(&self, id: OrderId) -> usize {
        // A cheap, well-distributed integer mix (splitmix64's finalizer),
        // not a security property: `OrderId`s are client-supplied but
        // uniform distribution is all a probe sequence needs here.
        let mut x = id;
        x ^= x >> 33;
        x = x.wrapping_mul(0xff51afd7ed558ccd);
        x ^= x >> 33;
        x = x.wrapping_mul(0xc4ceb9fe1a85ec53);
        x ^= x >> 33;
        (x as usize) & self.mask
    }

    pub fn contains_key(&self, id: OrderId) -> bool {
        self.get(id).is_some()
    }

    pub fn get(&self, id: OrderId) -> Option<&V> {
        let mut i = self.slot_for(id);
        loop {
            match &self.slots[i] {
                None => return None,
                Some((k, v)) if *k == id => return Some(v),
                _ => i = (i + 1) & self.mask,
            }
        }
    }

    /// Inserts `value` under `id`, overwriting any existing value for
    /// that `id` in place (matching `std::collections::HashMap::insert`
    /// semantics), never growing the table.
    pub fn insert(&mut self, id: OrderId, value: V) {
        let mut i = self.slot_for(id);
        loop {
            match &mut self.slots[i] {
                None => {
                    self.slots[i] = Some((id, value));
                    self.len += 1;
                    return;
                }
                Some((k, v)) if *k == id => {
                    *v = value;
                    return;
                }
                _ => i = (i + 1) & self.mask,
            }
        }
    }

    pub fn remove(&mut self, id: OrderId) -> Option<V> {
        let mut i = self.slot_for(id);
        loop {
            match &self.slots[i] {
                None => return None,
                Some((k, _)) if *k == id => {
                    let (_, v) = self.slots[i].take().expect("just matched Some above");
                    self.len -= 1;
                    self.backward_shift_from(i);
                    return Some(v);
                }
                _ => i = (i + 1) & self.mask,
            }
        }
    }

    pub fn keys(&self) -> impl Iterator<Item = OrderId> + '_ {
        self.slots.iter().filter_map(|slot| slot.as_ref().map(|(k, _)| *k))
    }

    /// After clearing slot `hole`, walk forward through the probe
    /// sequence and pull back any entry that can still be found by a
    /// lookup starting from its own ideal slot -- i.e. whose probe
    /// distance from `hole` is no shorter via `hole` than via its current
    /// position. This is what makes deletion tombstone-free.
    fn backward_shift_from(&mut self, mut hole: usize) {
        let mut probe = (hole + 1) & self.mask;
        while let Some((k, _)) = &self.slots[probe] {
            let ideal = self.slot_for(*k);
            let probe_to_ideal = probe.wrapping_sub(ideal) & self.mask;
            let hole_to_ideal = hole.wrapping_sub(ideal) & self.mask;
            if hole_to_ideal <= probe_to_ideal {
                self.slots[hole] = self.slots[probe].take();
                hole = probe;
            }
            probe = (probe + 1) & self.mask;
        }
    }
}
