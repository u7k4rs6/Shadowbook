//! A1-A10 ported from `reference/tests/attack_scenarios.rs`, run against
//! the optimized engine. Same assertions, same scenarios, different
//! engine: any divergence here is either an engine bug or a test that
//! needed adjusting for the API difference (`apply` returns `&[Event]`,
//! not `Vec<Event>`), never a semantic change.
//!
//! Also covers the two mechanisms Session 2 added and that A1-A10 do not
//! exercise at all: arena capacity (`RejectReason::ArenaFull`) and the
//! id retirement window (`RejectReason::DuplicateOrderId`). Both are
//! shared, both must behave identically to `reference`, and neither had
//! any engine-side coverage without these.

use engine::{check_invariants, Book};
use types::{Command, Config, Event, Kind, RejectReason, Side, DEFAULT_CONFIG};

fn wide_cfg() -> Config {
    DEFAULT_CONFIG
}

fn new_cmd(id: u64, account: u32, side: Side, kind: Kind, price: i64, qty: u64) -> Command {
    Command::New { id, account, side, kind, price, qty }
}

fn assert_no_violations(book: &Book) {
    let v = check_invariants(book);
    assert!(v.is_empty(), "invariant violations: {v:#?}");
}

fn rejected_reason(events: &[Event], id: u64) -> Option<RejectReason> {
    events.iter().find_map(|e| match e {
        Event::Rejected { id: eid, reason, .. } if *eid == id => Some(*reason),
        _ => None,
    })
}

/// A1: post-only that would cross must reject, both exactly at the
/// touch and through it.
#[test]
fn a1_post_only_would_cross() {
    let mut book = Book::new(wide_cfg());
    book.apply(new_cmd(1, 1, Side::Sell, Kind::Limit, 100, 10));

    let at_touch = book.apply(new_cmd(2, 2, Side::Buy, Kind::PostOnly, 100, 5));
    assert_eq!(rejected_reason(at_touch, 2), Some(RejectReason::WouldCross), "{at_touch:?}");

    let through_touch = book.apply(new_cmd(3, 2, Side::Buy, Kind::PostOnly, 101, 5));
    assert_eq!(rejected_reason(through_touch, 3), Some(RejectReason::WouldCross), "{through_touch:?}");

    assert_no_violations(&book);
}

/// A2: a cancel that races a fill must reject as UnknownOrder, not
/// cancel whatever now occupies the id's former slot.
#[test]
fn a2_cancel_races_fill() {
    let mut book = Book::new(wide_cfg());
    let resting = book.apply(new_cmd(1, 1, Side::Sell, Kind::Limit, 100, 10));
    let resting_seq = match resting[0] {
        Event::Accepted { seq, .. } => seq,
        _ => panic!("expected Accepted: {resting:?}"),
    };

    let sweep = book.apply(new_cmd(2, 2, Side::Buy, Kind::Market, 0, 10));
    assert!(sweep.iter().any(|e| matches!(e, Event::Fill { maker: 1, qty: 10, .. })));

    let cancel = book.apply(Command::Cancel { id: 1 });
    match cancel[0] {
        Event::Rejected { id: 1, reason: RejectReason::UnknownOrder, seq } => {
            assert_eq!(seq, resting_seq + 2, "cancel should be exactly two ingress commands after the resting order");
        }
        other => panic!("expected Rejected(UnknownOrder): {other:?}"),
    }

    assert_no_violations(&book);
}

/// A3: amending an order into a self-match must cancel the resting
/// counterpart (CancelResting) and leave the book uncrossed, without the
/// account trading against itself.
#[test]
fn a3_amend_into_self_match() {
    let mut book = Book::new(wide_cfg());
    book.apply(new_cmd(1, 1, Side::Buy, Kind::Limit, 100, 5)); // resting bid, account 1
    book.apply(new_cmd(2, 1, Side::Sell, Kind::Limit, 105, 5)); // resting ask, account 1

    let events = book.apply(Command::Amend { id: 2, new_price: 99, new_qty: 5 });

    assert!(
        !events.iter().any(|e| matches!(e, Event::Fill { maker: 1, .. } | Event::Fill { taker: 1, .. })),
        "account 1 filled against itself: {events:#?}"
    );
    assert!(events.contains(&Event::Cancelled { id: 1 }), "expected resting bid to be cancelled: {events:#?}");
    let amended_pos = events.iter().position(|e| matches!(e, Event::Amended { id: 2, lost_priority: true }));
    assert_eq!(amended_pos, Some(0), "Amended must be first: {events:#?}");

    assert_eq!(book.best_bid(), None);
    assert_eq!(book.best_ask(), Some(99));
    assert_no_violations(&book);
}

/// A4: FOK's dry run must apply the same self-match policy as real
/// execution, in both directions.
#[test]
fn a4_fok_dry_run_matches_self_match_policy() {
    // (a) accept: self-match bait in front, genuine liquidity behind it.
    let mut book = Book::new(wide_cfg());
    book.apply(new_cmd(1, 2, Side::Sell, Kind::Limit, 100, 10)); // self-match bait
    book.apply(new_cmd(2, 3, Side::Sell, Kind::Limit, 101, 10)); // genuine liquidity

    let events = book.apply(new_cmd(3, 2, Side::Buy, Kind::Fok, 101, 10));

    assert!(
        events.iter().any(|e| matches!(e, Event::Accepted { id: 3, .. })),
        "FOK should have been fillable via the deeper level: {events:#?}"
    );
    assert!(events.contains(&Event::Cancelled { id: 1 }), "self-match bait should be cancelled: {events:#?}");
    assert!(
        events.iter().any(|e| matches!(e, Event::Fill { maker: 2, taker: 3, qty: 10, .. })),
        "should have filled fully from the genuine liquidity: {events:#?}"
    );
    assert_no_violations(&book);

    // (b) reject: the only resting liquidity is self-match bait.
    let mut book2 = Book::new(wide_cfg());
    book2.apply(new_cmd(10, 5, Side::Sell, Kind::Limit, 200, 10)); // self-match bait, sole liquidity

    let events2 = book2.apply(new_cmd(11, 5, Side::Buy, Kind::Fok, 200, 10));

    assert_eq!(
        rejected_reason(events2, 11),
        Some(RejectReason::Unfillable),
        "an order whose only liquidity is itself must be rejected, not accepted then emptied: {events2:#?}"
    );
    assert!(
        !events2.iter().any(|e| matches!(e, Event::Cancelled { .. } | Event::Fill { .. })),
        "FOK rejection must mutate nothing -- the dry run's self-match skip must not leak into the real book: {events2:#?}"
    );
    assert!(book2.is_live(10));
    assert_eq!(book2.best_ask(), Some(200));
    assert_no_violations(&book2);
}

/// A5: market order into an empty book emits Cancelled with zero fills,
/// never Rejected, never a panic.
#[test]
fn a5_market_into_empty_book() {
    let mut book = Book::new(wide_cfg());
    let events = book.apply(new_cmd(1, 1, Side::Buy, Kind::Market, 0, 10));

    assert!(events.iter().any(|e| matches!(e, Event::Accepted { id: 1, .. })));
    assert!(events.contains(&Event::Cancelled { id: 1 }));
    assert!(!events.iter().any(|e| matches!(e, Event::Fill { .. } | Event::Rejected { .. })));

    assert_no_violations(&book);
}

/// A6: qty = u64::MAX at the highest permitted tick must not overflow a
/// notional computation. Mirrors `reference::notional`'s widening rule;
/// neither engine computes notional in its matching path, so this tests
/// the standalone widening utility both crates carry for exactly this
/// purpose.
#[test]
fn a6_notional_overflow_widens_to_i128() {
    let price: i64 = DEFAULT_CONFIG.tick_max();
    let qty: u64 = u64::MAX;

    let notional = engine::notional(price, qty);

    let expected = (price as i128) * (qty as i128);
    assert_eq!(notional, expected);
    assert!(notional > i64::MAX as i128, "notional should exceed i64::MAX, proving no truncation occurred");
}

/// A7: many partial fills, no tolerance. Uses the shared `EventAuditor`
/// (types crate), the same instrument the fuzz harness will use.
#[test]
fn a7_no_dust_across_many_partial_fills() {
    let mut book = Book::new(wide_cfg());
    let mut auditor = types::EventAuditor::new();
    let mut next_id = 1u64;

    for _ in 0..10_000 {
        let ask_id = next_id;
        next_id += 1;
        let cmd = new_cmd(ask_id, 1, Side::Sell, Kind::Limit, 100, 1);
        let events = book.apply(cmd);
        auditor.observe(&cmd, events);

        let buy_id = next_id;
        next_id += 1;
        let cmd = new_cmd(buy_id, 2, Side::Buy, Kind::Ioc, 100, 1);
        let events = book.apply(cmd);
        auditor.observe(&cmd, events);
    }

    assert!(auditor.violations.is_empty(), "{:#?}", auditor.violations);
    assert!(auditor.quantity_conserved());
    assert_eq!(auditor.buy_filled, 10_000);
    assert_eq!(auditor.sell_filled, 10_000);
    assert_no_violations(&book);
}

/// A8: sustained insert/cancel at the touch. Unlike `reference`, I8
/// (arena accounting) genuinely applies here and is checked by
/// `assert_no_violations` on every iteration alongside the live-count
/// bound.
#[test]
fn a8_quote_stuffing_never_exceeds_two_live_orders() {
    let mut book = Book::new(wide_cfg());

    for id in 1u64..=10_000 {
        book.apply(new_cmd(id, 1, Side::Buy, Kind::Limit, 100, 1));
        assert!(book.live_count() <= 2, "live count grew beyond the stuffing scenario's bound");
        book.apply(Command::Cancel { id });
        assert_no_violations(&book);
    }

    assert_eq!(book.live_count(), 0);
}

/// A9: emptying the book with one enormous market order must leave
/// best_bid/best_ask as None and the live set at zero -- the bitmap scan
/// walking off the end of the summary array, or a stale bit surviving an
/// empty level, is exactly what this and I5 are watching for -- and a
/// subsequent insert must correctly become the new touch.
#[test]
fn a9_full_sweep_and_rebuild() {
    let mut book = Book::new(wide_cfg());
    for i in 0..5u64 {
        book.apply(new_cmd(i + 1, 1, Side::Sell, Kind::Limit, 100 + i as i64, 10));
    }

    book.apply(new_cmd(100, 2, Side::Buy, Kind::Market, 0, 1_000));

    assert_eq!(book.best_bid(), None);
    assert_eq!(book.best_ask(), None);
    assert_eq!(book.live_count(), 0);
    assert_no_violations(&book);

    book.apply(new_cmd(200, 3, Side::Buy, Kind::Limit, 50, 5));
    assert_eq!(book.best_bid(), Some(50));
    assert_no_violations(&book);
}

/// A10: orders at exactly the band edges accept; one tick outside on
/// either side rejects.
#[test]
fn a10_price_band_boundary() {
    assert_eq!(DEFAULT_CONFIG.tick_max(), (DEFAULT_CONFIG.n_ticks as i64) - 1);

    let mut book = Book::new(DEFAULT_CONFIG);
    let max_tick = DEFAULT_CONFIG.tick_max();

    let at_min = book.apply(new_cmd(1, 1, Side::Buy, Kind::Limit, 0, 1));
    assert!(at_min.iter().any(|e| matches!(e, Event::Accepted { .. })), "{at_min:?}");

    let at_max = book.apply(new_cmd(2, 1, Side::Sell, Kind::Limit, max_tick, 1));
    assert!(at_max.iter().any(|e| matches!(e, Event::Accepted { .. })), "{at_max:?}");

    let below_min = book.apply(new_cmd(3, 1, Side::Buy, Kind::Limit, -1, 1));
    assert_eq!(rejected_reason(below_min, 3), Some(RejectReason::PriceOutOfBand), "{below_min:?}");

    let above_max = book.apply(new_cmd(4, 1, Side::Sell, Kind::Limit, max_tick + 1, 1));
    assert_eq!(rejected_reason(above_max, 4), Some(RejectReason::PriceOutOfBand), "{above_max:?}");

    assert_no_violations(&book);
}

/// Session 2: arena capacity enforced at New ingress, before matching,
/// before any mutation, mirroring `reference::arena_full_rejects_new_
/// and_mutates_nothing` exactly, including the conservative rule: a
/// marketable order that would have fully filled without ever resting is
/// still rejected when the arena is full.
#[test]
fn arena_full_rejects_new_and_mutates_nothing() {
    let mut book = Book::new(wide_cfg());
    let capacity = wide_cfg().capacity;

    for id in 1..=capacity as u64 {
        let events = book.apply(new_cmd(id, 1, Side::Buy, Kind::Limit, 100, 1));
        assert!(events.iter().any(|e| matches!(e, Event::Accepted { .. })), "order {id} should have rested: {events:?}");
    }
    assert_eq!(book.live_count(), capacity);

    let over = book.apply(new_cmd(999, 2, Side::Sell, Kind::Market, 0, 1));
    assert_eq!(rejected_reason(over, 999), Some(RejectReason::ArenaFull), "{over:?}");
    assert!(
        !over.iter().any(|e| matches!(e, Event::Fill { .. } | Event::Cancelled { .. } | Event::Accepted { .. })),
        "a rejected New must mutate nothing: {over:?}"
    );
    assert_eq!(book.live_count(), capacity, "rejected New must not touch live count");

    book.apply(Command::Cancel { id: 1 });
    assert_eq!(book.live_count(), capacity - 1);
    let now_fits = book.apply(new_cmd(1000, 3, Side::Buy, Kind::Limit, 100, 1));
    assert!(now_fits.iter().any(|e| matches!(e, Event::Accepted { .. })), "{now_fits:?}");

    assert_no_violations(&book);
}

/// Session 2: the F-001 ABA guard, mirroring `reference::duplicate_
/// order_id_rejects_reuse_within_window_then_allows_after_eviction`.
#[test]
fn duplicate_order_id_rejects_reuse_within_window_then_allows_after_eviction() {
    let mut book = Book::new(wide_cfg());
    let window = wide_cfg().id_retirement_window;

    book.apply(new_cmd(1, 1, Side::Buy, Kind::Limit, 100, 1));
    book.apply(Command::Cancel { id: 1 });
    assert!(!book.is_live(1));

    let reused = book.apply(new_cmd(1, 2, Side::Sell, Kind::Limit, 100, 1));
    assert_eq!(rejected_reason(reused, 1), Some(RejectReason::DuplicateOrderId), "{reused:?}");
    assert!(!book.is_live(1));

    for id in 10_000..10_000 + window as u64 {
        book.apply(new_cmd(id, 1, Side::Buy, Kind::Limit, 100, 1));
        book.apply(Command::Cancel { id });
    }

    let reused_after_eviction = book.apply(new_cmd(1, 2, Side::Sell, Kind::Limit, 100, 1));
    assert!(
        reused_after_eviction.iter().any(|e| matches!(e, Event::Accepted { .. })),
        "id 1 should be reusable once it ages out of the retirement window: {reused_after_eviction:?}"
    );

    assert_no_violations(&book);
}
