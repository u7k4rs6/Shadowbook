//! Shared `Command` / `Event` vocabulary for `Shadowbook`.
//!
//! Both the `reference` (oracle) engine and the `engine` (optimized) engine
//! speak this protocol. Neither crate is allowed to define its own
//! divergent notion of what a command or an event is, which would make the
//! differential fuzzer meaningless.

#![forbid(unsafe_code)]

use std::collections::HashMap;

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

/// N_TICKS = 65536. Ticks are absolute: `TickIdx = tick as usize`, no
/// offset arithmetic. The band is deliberately anchored at zero so the
/// sharp edge (a `usize` underflow near the low boundary) stays directly
/// reachable instead of being hidden behind an offset subtraction that
/// would be correct by construction.
pub const N_TICKS: usize = 65536;

/// Price band both engines must reject identically outside `[tick_min,
/// tick_max]`, or the differential fuzzer will report every out-of-band
/// order as a divergence rather than a real bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub tick_min: Price,
    pub tick_max: Price,
}

impl Config {
    pub fn contains(&self, price: Price) -> bool {
        price >= self.tick_min && price <= self.tick_max
    }
}

/// The band pinned by the spec: `[0, N_TICKS - 1]`. Tick 0 is legal.
pub const DEFAULT_CONFIG: Config = Config { tick_min: 0, tick_max: (N_TICKS as Price) - 1 };

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    UnknownOrder,
    DuplicateId,
    InvalidQuantity,
    /// Outside `[Config::tick_min, Config::tick_max]`.
    PriceOutOfBand,
    WouldCross,
    Unfillable,
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
