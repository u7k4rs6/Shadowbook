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
            Command::New { id, side, kind, price, .. } => {
                if events.iter().any(|e| matches!(e, Event::Accepted { id: eid, .. } if eid == id)) {
                    self.orders.insert(*id, OrderContext { side: *side, kind: *kind, price: *price });
                }
            }
            Command::Amend { id, new_price, .. } => {
                if events.iter().any(|e| matches!(e, Event::Amended { id: eid, .. } if eid == id)) {
                    if let Some(ctx) = self.orders.get_mut(id) {
                        ctx.price = *new_price;
                    }
                }
            }
            Command::Cancel { .. } => {}
        }

        for e in events {
            if let Event::Fill { maker, taker, price, qty } = e {
                self.check_fill(*maker, *taker, *price, *qty);
            }
        }
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
                self.credit(ctx.side, qty);
            }
            None => self.violations.push(format!("fill references unknown maker {maker}")),
        }

        match self.orders.get(&taker).copied() {
            Some(ctx) => {
                self.check_limit(taker, ctx, price, "I3 (taker)");
                self.credit(ctx.side, qty);
            }
            None => self.violations.push(format!("fill references unknown taker {taker}")),
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
/// why `members` is a hand-rolled `FixedIdSet`, not `std::collections::
/// HashSet`. A `HashSet` bounded the same way (evict-before-insert, never
/// exceeding `window` live entries) still grows internally under
/// sustained insert/remove churn: hashbrown's tombstone accounting
/// eventually forces a table resize even though occupancy never grows,
/// which is exactly the kind of thing `engine`'s zero-allocation test
/// exists to catch, and did. `FixedIdSet` uses backward-shift deletion
/// (no tombstones), so there is nothing to accumulate and no code path
/// that could ever reallocate it.
#[derive(Debug, Clone)]
pub struct RetirementRing {
    window: usize,
    order: VecDeque<OrderId>,
    members: FixedIdSet,
}

impl RetirementRing {
    pub fn new(window: usize) -> Self {
        RetirementRing { window, order: VecDeque::with_capacity(window), members: FixedIdSet::with_capacity(window) }
    }

    pub fn contains(&self, id: OrderId) -> bool {
        self.members.contains(id)
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
        if self.members.contains(id) {
            return;
        }
        if self.order.len() >= self.window {
            if let Some(evicted) = self.order.pop_front() {
                self.members.remove(evicted);
            }
        }
        self.order.push_back(id);
        self.members.insert(id);
    }
}

/// Fixed-capacity open-addressing set of `OrderId`, linear probing with
/// backward-shift deletion (the Robin-Hood-hashing removal algorithm):
/// removing an entry immediately slides later-probed entries backward
/// into the freed slot rather than leaving a tombstone behind, so there
/// is nothing for repeated insert/remove cycles to accumulate and no
/// growth heuristic to ever trigger. Capacity is fixed at construction
/// (`slots.len()` is a power of two, sized for a max ~50% load factor)
/// and never changes.
#[derive(Debug, Clone)]
struct FixedIdSet {
    slots: Vec<Option<OrderId>>,
    mask: usize,
    len: usize,
}

impl FixedIdSet {
    fn with_capacity(min_capacity: usize) -> Self {
        let cap = (min_capacity.max(1) * 2).next_power_of_two();
        FixedIdSet { slots: vec![None; cap], mask: cap - 1, len: 0 }
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

    fn contains(&self, id: OrderId) -> bool {
        let mut i = self.slot_for(id);
        loop {
            match self.slots[i] {
                None => return false,
                Some(v) if v == id => return true,
                _ => i = (i + 1) & self.mask,
            }
        }
    }

    fn insert(&mut self, id: OrderId) {
        let mut i = self.slot_for(id);
        loop {
            match self.slots[i] {
                None => {
                    self.slots[i] = Some(id);
                    self.len += 1;
                    return;
                }
                Some(v) if v == id => return,
                _ => i = (i + 1) & self.mask,
            }
        }
    }

    fn remove(&mut self, id: OrderId) {
        let mut i = self.slot_for(id);
        loop {
            match self.slots[i] {
                None => return,
                Some(v) if v == id => {
                    self.slots[i] = None;
                    self.len -= 1;
                    self.backward_shift_from(i);
                    return;
                }
                _ => i = (i + 1) & self.mask,
            }
        }
    }

    /// After clearing slot `hole`, walk forward through the probe
    /// sequence and pull back any entry that can still be found by a
    /// lookup starting from its own ideal slot -- i.e. whose probe
    /// distance from `hole` is no shorter via `hole` than via its current
    /// position. This is what makes deletion tombstone-free.
    fn backward_shift_from(&mut self, mut hole: usize) {
        let mut probe = (hole + 1) & self.mask;
        while let Some(v) = self.slots[probe] {
            let ideal = self.slot_for(v);
            let probe_to_ideal = probe.wrapping_sub(ideal) & self.mask;
            let hole_to_ideal = hole.wrapping_sub(ideal) & self.mask;
            if hole_to_ideal <= probe_to_ideal {
                self.slots[hole] = Some(v);
                self.slots[probe] = None;
                hole = probe;
            }
            probe = (probe + 1) & self.mask;
        }
    }
}
