//! The command generator, with its own shadow reference engine as an
//! oracle for liveness.
//!
//! A generator that keeps its own naive list of candidate ids diverges
//! from the real book: first in size, if its pool is unbounded against a
//! fixed arena, then in membership, because nothing tells it when an
//! order fills. The fix here is structural, not a tuning fix: the
//! generator owns a real `reference::RefBook`, applies every command it
//! emits to that shadow book before returning it, and asks the shadow
//! book itself -- `resting_orders()`, `retired_ids()` -- for the current
//! truth every time it needs to pick a Cancel/Amend target. There is
//! nothing to derive and therefore nothing that can drift: the shadow
//! book's state *is* the generator's model, not a re-implementation of
//! it that could disagree.

use std::collections::HashMap;

use rand::Rng;

use reference::{RefBook, RestingOrderView};
use types::{AccountId, Command, Config, Event, Kind, OrderId, Price, Qty, RejectReason, Side};

/// Guaranteed-never-issued ids for Cancel's "never-seen id" bucket live in
/// a disjoint high range, so they can never collide with the recycling
/// pool New/Cancel/Amend otherwise draw target ids from.
const FRESH_ID_BASE: OrderId = 1_000_000_000;

/// Top-level command-type weights, `new_pct + cancel_pct + amend_pct ==
/// 100`. The default (`BALANCED`) is what every calibration run and the
/// main 10^8 run used. A generator this thin (Cancel succeeding ~56% of
/// the time) essentially never drives the book to `Config::capacity`
/// simultaneously-live orders, so `RejectReason::ArenaFull` needs its own
/// generator shape, not a tuning tweak to the balanced one: `SATURATING`
/// biases hard toward New and starves Cancel, so resting orders
/// accumulate faster than they're removed.
#[derive(Debug, Clone, Copy)]
pub struct CommandMix {
    pub new_pct: u32,
    pub cancel_pct: u32,
    pub amend_pct: u32,
}

impl CommandMix {
    pub const BALANCED: CommandMix = CommandMix { new_pct: 60, cancel_pct: 25, amend_pct: 15 };
    pub const SATURATING: CommandMix = CommandMix { new_pct: 80, cancel_pct: 10, amend_pct: 10 };
}

#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub new_attempted: u64,
    pub new_accepted: u64,
    pub new_rejects: HashMap<RejectReason, u64>,
    pub kind_counts: HashMap<Kind, u64>,

    pub cancel_attempted: u64,
    pub cancel_succeeded: u64,
    pub cancel_rejects: HashMap<RejectReason, u64>,

    pub amend_attempted: u64,
    pub amend_succeeded: u64,
    pub amend_lost_priority: u64,
    pub amend_kept_priority: u64,
    pub amend_rejects: HashMap<RejectReason, u64>,
    pub crossing_amend_count: u64,
}

impl Stats {
    pub fn merge(&mut self, other: &Stats) {
        self.new_attempted += other.new_attempted;
        self.new_accepted += other.new_accepted;
        for (k, v) in &other.new_rejects {
            *self.new_rejects.entry(*k).or_insert(0) += v;
        }
        for (k, v) in &other.kind_counts {
            *self.kind_counts.entry(*k).or_insert(0) += v;
        }
        self.cancel_attempted += other.cancel_attempted;
        self.cancel_succeeded += other.cancel_succeeded;
        for (k, v) in &other.cancel_rejects {
            *self.cancel_rejects.entry(*k).or_insert(0) += v;
        }
        self.amend_attempted += other.amend_attempted;
        self.amend_succeeded += other.amend_succeeded;
        self.amend_lost_priority += other.amend_lost_priority;
        self.amend_kept_priority += other.amend_kept_priority;
        for (k, v) in &other.amend_rejects {
            *self.amend_rejects.entry(*k).or_insert(0) += v;
        }
        self.crossing_amend_count += other.crossing_amend_count;
    }

    pub fn total_commands(&self) -> u64 {
        self.new_attempted + self.cancel_attempted + self.amend_attempted
    }

    pub fn report(&self) -> String {
        let pct = |num: u64, den: u64| if den == 0 { 0.0 } else { (num as f64 / den as f64) * 100.0 };
        let mut out = String::new();
        out.push_str(&format!(
            "New: {} attempted, {:.1}% accepted, rejects: {:?}\n",
            self.new_attempted,
            pct(self.new_accepted, self.new_attempted),
            self.new_rejects
        ));
        out.push_str(&format!("  kind mix: {:?}\n", self.kind_counts));
        out.push_str(&format!(
            "Cancel: {} attempted, {:.1}% succeeded, rejects: {:?}\n",
            self.cancel_attempted,
            pct(self.cancel_succeeded, self.cancel_attempted),
            self.cancel_rejects
        ));
        out.push_str(&format!(
            "Amend: {} attempted, {:.1}% succeeded ({} lost priority, {} kept), {:.1}% of amends crossed, rejects: {:?}\n",
            self.amend_attempted,
            pct(self.amend_succeeded, self.amend_attempted),
            self.amend_lost_priority,
            self.amend_kept_priority,
            pct(self.crossing_amend_count, self.amend_attempted),
            self.amend_rejects
        ));
        out
    }
}

pub struct Generator {
    shadow: RefBook,
    cfg: Config,
    mix: CommandMix,
    accounts: [AccountId; 4],
    pool_size: u64,
    fresh_counter: u64,
    pub stats: Stats,
}

impl Generator {
    pub fn new(cfg: Config) -> Self {
        Self::with_mix(cfg, CommandMix::BALANCED)
    }

    pub fn with_mix(cfg: Config, mix: CommandMix) -> Self {
        Self::with_mix_and_pool(cfg, mix, (cfg.capacity as u64 * 2).max(16))
    }

    /// `pool_size` matters more than it looks for a saturating mix: a
    /// pool only ~2x capacity is exactly what makes New collide with
    /// live/retired ids constantly under the balanced mix (the point,
    /// there), but that same collision pressure is what stops
    /// `CommandMix::SATURATING` from ever finishing the climb to
    /// capacity -- the closer the book gets to full, the more of the
    /// (still-narrow) pool is already live, so a New draw becomes
    /// increasingly likely to bounce as DuplicateId/DuplicateOrderId
    /// instead of adding the next resting order. A wider pool removes
    /// that self-limiting dynamic without changing `Config::capacity`
    /// itself.
    pub fn with_mix_and_pool(cfg: Config, mix: CommandMix, pool_size: u64) -> Self {
        assert_eq!(mix.new_pct + mix.cancel_pct + mix.amend_pct, 100, "CommandMix must sum to 100: {mix:?}");
        Generator {
            shadow: RefBook::new(cfg),
            cfg,
            mix,
            accounts: [1, 2, 3, 4],
            pool_size,
            fresh_counter: FRESH_ID_BASE,
            stats: Stats::default(),
        }
    }

    /// The shadow book's own digest, exposed so a runner can additionally
    /// cross-check its own bookkeeping against the shadow if desired.
    pub fn shadow(&self) -> &RefBook {
        &self.shadow
    }

    fn next_fresh_id(&mut self) -> OrderId {
        let id = self.fresh_counter;
        self.fresh_counter += 1;
        id
    }

    fn current_touch(&self) -> Price {
        match (self.shadow.best_bid(), self.shadow.best_ask()) {
            (Some(b), Some(a)) => (b + a) / 2,
            (Some(b), None) => b,
            (None, Some(a)) => a,
            (None, None) => self.cfg.tick_max() / 2,
        }
    }

    /// Ticks 0 and 1 (the low-edge underflow-adjacent region), the
    /// summary word boundaries at 63/64 and 127/128 (bitmap-scan
    /// off-by-one territory), the band's top edge, and touch +/- 64 (a
    /// full word away from the action).
    fn adversarial_prices(&self) -> [Price; 9] {
        let max = self.cfg.tick_max();
        let touch = self.current_touch();
        [
            0,
            1,
            63.min(max),
            64.min(max),
            127.min(max),
            128.min(max),
            max,
            (touch - 64).max(0),
            (touch + 64).min(max),
        ]
    }

    fn gen_price(&self, rng: &mut impl Rng) -> Price {
        let roll = rng.gen_range(0..100);
        if roll < 85 {
            let touch = self.current_touch();
            let offset: i64 = rng.gen_range(-3..=3);
            (touch + offset).clamp(0, self.cfg.tick_max())
        } else if roll < 95 {
            rng.gen_range(0..=self.cfg.tick_max())
        } else {
            let choices = self.adversarial_prices();
            choices[rng.gen_range(0..choices.len())]
        }
    }

    fn gen_qty(&self, rng: &mut impl Rng) -> Qty {
        let roll = rng.gen_range(0..100);
        if roll < 40 {
            1
        } else if roll < 50 {
            u64::MAX / 2
        } else {
            rng.gen_range(1..=20)
        }
    }

    fn gen_kind(&self, rng: &mut impl Rng) -> Kind {
        match rng.gen_range(0..100) {
            0..=59 => Kind::Limit,
            60..=74 => Kind::PostOnly,
            75..=84 => Kind::Ioc,
            85..=94 => Kind::Market,
            _ => Kind::Fok,
        }
    }

    fn gen_account(&self, rng: &mut impl Rng) -> AccountId {
        self.accounts[rng.gen_range(0..self.accounts.len())]
    }

    /// New's id is drawn from a pool only ~2x the arena's capacity, not a
    /// fresh id every time: a finite, client-sized id space means New
    /// naturally collides with currently-live ids (DuplicateId) and
    /// recently-retired ones (DuplicateOrderId) at a realistic rate,
    /// without any special-casing to force it.
    fn gen_new(&mut self, rng: &mut impl Rng) -> Command {
        Command::New {
            id: rng.gen_range(1..=self.pool_size),
            account: self.gen_account(rng),
            side: if rng.gen_bool(0.5) { Side::Buy } else { Side::Sell },
            kind: self.gen_kind(rng),
            price: self.gen_price(rng),
            qty: self.gen_qty(rng),
        }
    }

    /// 60% of cancels target the exact live set, 20% the retired ring
    /// (deliberately driving the reused-id scenario), 20% a guaranteed
    /// never-issued id. All three pools are read directly from the
    /// shadow book, not tracked independently.
    fn gen_cancel(&mut self, rng: &mut impl Rng) -> Command {
        let roll = rng.gen_range(0..25);
        let id = if roll < 15 {
            let live = self.shadow.resting_orders();
            if live.is_empty() { rng.gen_range(1..=self.pool_size) } else { live[rng.gen_range(0..live.len())].id }
        } else if roll < 20 {
            let retired = self.shadow.retired_ids();
            if retired.is_empty() { rng.gen_range(1..=self.pool_size) } else { retired[rng.gen_range(0..retired.len())] }
        } else {
            self.next_fresh_id()
        };
        Command::Cancel { id }
    }

    /// 95% of amends target a real live order (read from the shadow, so
    /// the quantity/price deltas below are computed against ground
    /// truth); 5% target a random pool id regardless, for some coverage
    /// of amend-on-a-not-currently-live id. Quantity is weighted toward
    /// remaining +/- 1 with real weight on the deliberately-invalid zero.
    /// Price is weighted toward small moves but with a full re-roll often
    /// enough to keep the repricing-amend-that-crosses rate healthy
    /// (needed to reach the Fill-before-Amended calibration bug).
    fn gen_amend(&mut self, rng: &mut impl Rng) -> Command {
        let resting = self.shadow.resting_orders();
        let target = if !resting.is_empty() && rng.gen_range(0..100) < 95 {
            resting[rng.gen_range(0..resting.len())]
        } else {
            RestingOrderView { id: rng.gen_range(1..=self.pool_size), side: Side::Buy, kind: Kind::Limit, price: self.cfg.tick_max() / 2, remaining: 1 }
        };

        let new_qty = match rng.gen_range(0..100) {
            0..=9 => 0,
            10..=44 => target.remaining.saturating_sub(1).max(1),
            45..=79 => target.remaining.saturating_add(1),
            _ => rng.gen_range(1..=20),
        };

        let new_price = match rng.gen_range(0..100) {
            0..=39 => target.price,
            40..=59 => (target.price + rng.gen_range(-3..=3)).clamp(0, self.cfg.tick_max()),
            _ => self.gen_price(rng),
        };

        Command::Amend { id: target.id, new_price, new_qty }
    }

    /// Picks and returns the next command, and applies it to the shadow
    /// book first so the *next* call sees an up-to-date live/retired
    /// picture. The two real engines under test never see the shadow;
    /// they receive exactly the `Command` this returns.
    pub fn next_command(&mut self, rng: &mut impl Rng) -> Command {
        let roll = rng.gen_range(0..100);
        let cmd = if roll < self.mix.new_pct {
            self.gen_new(rng)
        } else if roll < self.mix.new_pct + self.mix.cancel_pct {
            self.gen_cancel(rng)
        } else {
            self.gen_amend(rng)
        };

        let events = self.shadow.apply(cmd);
        self.record_stats(&cmd, &events);
        // The shadow is a throwaway liveness model, never replayed or
        // introspected via its own command log -- only its *current*
        // state (`resting_orders`, `retired_ids`, `best_bid`/`best_ask`)
        // is ever read. Clearing the log after every command keeps a
        // long fuzz run from holding a multi-gigabyte history nothing
        // will ever use.
        self.shadow.clear_log();
        cmd
    }

    fn record_stats(&mut self, cmd: &Command, events: &[Event]) {
        match cmd {
            Command::New { kind, .. } => {
                self.stats.new_attempted += 1;
                *self.stats.kind_counts.entry(*kind).or_insert(0) += 1;
                for e in events {
                    match e {
                        Event::Accepted { .. } => self.stats.new_accepted += 1,
                        Event::Rejected { reason, .. } => *self.stats.new_rejects.entry(*reason).or_insert(0) += 1,
                        _ => {}
                    }
                }
            }
            Command::Cancel { .. } => {
                self.stats.cancel_attempted += 1;
                for e in events {
                    match e {
                        Event::Cancelled { .. } => self.stats.cancel_succeeded += 1,
                        Event::Rejected { reason, .. } => *self.stats.cancel_rejects.entry(*reason).or_insert(0) += 1,
                        _ => {}
                    }
                }
            }
            Command::Amend { .. } => {
                self.stats.amend_attempted += 1;
                let mut has_fill = false;
                for e in events {
                    match e {
                        Event::Amended { lost_priority, .. } => {
                            self.stats.amend_succeeded += 1;
                            if *lost_priority {
                                self.stats.amend_lost_priority += 1;
                            } else {
                                self.stats.amend_kept_priority += 1;
                            }
                        }
                        Event::Rejected { reason, .. } => *self.stats.amend_rejects.entry(*reason).or_insert(0) += 1,
                        Event::Fill { .. } => has_fill = true,
                        _ => {}
                    }
                }
                if has_fill {
                    self.stats.crossing_amend_count += 1;
                }
            }
        }
    }
}
