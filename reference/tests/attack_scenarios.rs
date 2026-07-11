//! Hand-written A1-A10 attack scenarios from Adversarial Test Plan section
//! 4, run against the reference engine only, plus the two additional
//! properties Session 1's correction round called out explicitly: clone
//! totality (Task 4) and the amend-quantity-is-remaining decision (Task
//! 4). See SESSION_1_SUMMARY.md / SESSION_2_SUMMARY.md for the mutation
//! calibration table proving each of these actually detects the bug it
//! claims to.

use reference::{check_invariants, RefBook};
use types::{Command, Config, Event, Kind, RejectReason, Side, DEFAULT_CONFIG};

fn wide_cfg() -> Config {
    DEFAULT_CONFIG
}

fn new_cmd(id: u64, account: u32, side: Side, kind: Kind, price: i64, qty: u64) -> Command {
    Command::New { id, account, side, kind, price, qty }
}

fn assert_no_violations(book: &RefBook) {
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
    let mut book = RefBook::new(wide_cfg());
    book.apply(new_cmd(1, 1, Side::Sell, Kind::Limit, 100, 10));

    let at_touch = book.apply(new_cmd(2, 2, Side::Buy, Kind::PostOnly, 100, 5));
    assert_eq!(rejected_reason(&at_touch, 2), Some(RejectReason::WouldCross), "{at_touch:?}");

    let through_touch = book.apply(new_cmd(3, 2, Side::Buy, Kind::PostOnly, 101, 5));
    assert_eq!(rejected_reason(&through_touch, 3), Some(RejectReason::WouldCross), "{through_touch:?}");

    assert_no_violations(&book);
}

/// A2: a cancel that races a fill must reject as UnknownOrder, not
/// cancel whatever now occupies the id's former slot. Also checks the
/// scenario's own premise -- the cancel's ingress seq is exactly one
/// past the New that triggered the sweep -- since that premise is only
/// even expressible now that rejected commands consume sequence space.
#[test]
fn a2_cancel_races_fill() {
    let mut book = RefBook::new(wide_cfg());
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
/// account trading against itself. This is the case where a naive
/// implementation holding an iterator into the bid side while amending
/// the ask would corrupt itself.
#[test]
fn a3_amend_into_self_match() {
    let mut book = RefBook::new(wide_cfg());
    book.apply(new_cmd(1, 1, Side::Buy, Kind::Limit, 100, 5)); // resting bid, account 1
    book.apply(new_cmd(2, 1, Side::Sell, Kind::Limit, 105, 5)); // resting ask, account 1

    let events = book.apply(Command::Amend { id: 2, new_price: 99, new_qty: 5 });

    // The account must never fill against itself.
    assert!(
        !events.iter().any(|e| matches!(e, Event::Fill { maker: 1, .. } | Event::Fill { taker: 1, .. })),
        "account 1 filled against itself: {events:#?}"
    );
    // The resting bid must have been cancelled via self-match prevention,
    // not silently left in the book or matched.
    assert!(events.contains(&Event::Cancelled { id: 1 }), "expected resting bid to be cancelled: {events:#?}");
    // Amended must be emitted before any Fill/Cancelled it causes, never
    // interleaved after them.
    let amended_pos = events.iter().position(|e| matches!(e, Event::Amended { id: 2, lost_priority: true }));
    assert_eq!(amended_pos, Some(0), "Amended must be first: {events:#?}");

    assert_eq!(book.best_bid(), None);
    assert_eq!(book.best_ask(), Some(99));
    assert_no_violations(&book);
}

/// A4: FOK's dry run must apply the same self-match policy as real
/// execution, in both directions.
///
/// (a) A naive dry run that *undercounts* self-match-skippable liquidity
///     would wrongly reject an order that's actually fillable from a
///     deeper level.
/// (b) A naive dry run that *overcounts* -- summing resting quantity
///     without simulating the self-match cancellation -- would wrongly
///     accept an order whose only "liquidity" is itself, via CancelResting.
///     (b) also exercises the Task 4 rollback decision: since the dry
///     run's self-match cancellations happen on a discarded clone, the
///     bait order must still be resting in the *real* book after the
///     FOK is rejected.
#[test]
fn a4_fok_dry_run_matches_self_match_policy() {
    // (a) accept: self-match bait in front, genuine liquidity behind it.
    let mut book = RefBook::new(wide_cfg());
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

    // (b) reject: the only resting liquidity is self-match bait, so the
    // true fillable quantity is zero.
    let mut book2 = RefBook::new(wide_cfg());
    book2.apply(new_cmd(10, 5, Side::Sell, Kind::Limit, 200, 10)); // self-match bait, sole liquidity

    let events2 = book2.apply(new_cmd(11, 5, Side::Buy, Kind::Fok, 200, 10));

    assert_eq!(
        rejected_reason(&events2, 11),
        Some(RejectReason::Unfillable),
        "an order whose only liquidity is itself must be rejected, not accepted then emptied: {events2:#?}"
    );
    assert!(
        !events2.iter().any(|e| matches!(e, Event::Cancelled { .. } | Event::Fill { .. })),
        "FOK rejection must mutate nothing -- the dry run's self-match cancellation must not leak into the real book: {events2:#?}"
    );
    // The bait order must still be live and resting, proving the dry
    // run's self-match cancellation happened only on the discarded clone.
    assert!(book2.is_live(10));
    assert_eq!(book2.best_ask(), Some(200));
    assert_no_violations(&book2);
}

/// A5: market order into an empty book emits Cancelled with zero fills,
/// never Rejected, never a panic.
#[test]
fn a5_market_into_empty_book() {
    let mut book = RefBook::new(wide_cfg());
    let events = book.apply(new_cmd(1, 1, Side::Buy, Kind::Market, 0, 10));

    assert!(events.iter().any(|e| matches!(e, Event::Accepted { id: 1, .. })));
    assert!(events.contains(&Event::Cancelled { id: 1 }));
    assert!(!events.iter().any(|e| matches!(e, Event::Fill { .. } | Event::Rejected { .. })));

    assert_no_violations(&book);
}

/// A6: qty = u64::MAX at the highest permitted tick must not overflow a
/// notional computation. Invariant-free: needs an explicit, dedicated
/// test on the widening arithmetic itself, since the reference engine
/// would overflow identically to a naive engine and the differential
/// check alone would never catch it.
#[test]
fn a6_notional_overflow_widens_to_i128() {
    let price: i64 = DEFAULT_CONFIG.tick_max;
    let qty: u64 = u64::MAX;

    let notional = reference::notional(price, qty);

    let expected = (price as i128) * (qty as i128);
    assert_eq!(notional, expected);
    assert!(notional > i64::MAX as i128, "notional should exceed i64::MAX, proving no truncation occurred");
}

/// A7: many partial fills, no tolerance. Every lot that leaves the buy
/// side must arrive at the sell side, exactly. Uses the shared
/// `EventAuditor` (types crate) rather than hand-rolled bookkeeping, so
/// this exercises the same machinery the fuzz harness will use.
#[test]
fn a7_no_dust_across_many_partial_fills() {
    let mut book = RefBook::new(wide_cfg());
    let mut auditor = types::EventAuditor::new();
    let mut next_id = 1u64;

    for _ in 0..10_000 {
        let ask_id = next_id;
        next_id += 1;
        let cmd = new_cmd(ask_id, 1, Side::Sell, Kind::Limit, 100, 1);
        let events = book.apply(cmd);
        auditor.observe(&cmd, &events);

        let buy_id = next_id;
        next_id += 1;
        let cmd = new_cmd(buy_id, 2, Side::Buy, Kind::Ioc, 100, 1);
        let events = book.apply(cmd);
        auditor.observe(&cmd, &events);
    }

    assert!(auditor.violations.is_empty(), "{:#?}", auditor.violations);
    assert!(auditor.quantity_conserved());
    assert_eq!(auditor.buy_filled, 10_000);
    assert_eq!(auditor.sell_filled, 10_000);
    assert_no_violations(&book);
}

/// A8: sustained insert/cancel at the touch. I8 (arena accounting) does
/// not apply to the reference engine (no arena) and tail latency is a
/// Day 4 benchmark concern, not a Day 1 correctness test. What *is*
/// checkable here on the reference engine: invariants keep holding and
/// the live set never exceeds the two orders the scenario allows.
#[test]
fn a8_quote_stuffing_never_exceeds_two_live_orders() {
    let mut book = RefBook::new(wide_cfg());

    for id in 1u64..=10_000 {
        book.apply(new_cmd(id, 1, Side::Buy, Kind::Limit, 100, 1));
        assert!(book.live_count() <= 2, "live count grew beyond the stuffing scenario's bound");
        book.apply(Command::Cancel { id });
        assert_no_violations(&book);
    }

    assert_eq!(book.live_count(), 0);
}

/// A9: emptying the book with one enormous market order must leave
/// best_bid/best_ask as None and the live set at zero, and a subsequent
/// insert must correctly become the new touch.
#[test]
fn a9_full_sweep_and_rebuild() {
    let mut book = RefBook::new(wide_cfg());
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
/// either side rejects. Band is pinned to `[0, N_TICKS - 1]` = `[0,
/// 65535]`, absolute ticks, no offset arithmetic -- so this test also
/// pins the low edge at zero rather than some arbitrary negative number,
/// keeping the usize-underflow-adjacent edge reachable for the engine
/// crate later.
#[test]
fn a10_price_band_boundary() {
    assert_eq!(DEFAULT_CONFIG.tick_min, 0);
    assert_eq!(DEFAULT_CONFIG.tick_max, (types::N_TICKS as i64) - 1);

    let mut book = RefBook::new(DEFAULT_CONFIG);

    let at_min = book.apply(new_cmd(1, 1, Side::Buy, Kind::Limit, 0, 1));
    assert!(at_min.iter().any(|e| matches!(e, Event::Accepted { .. })), "{at_min:?}");

    let at_max = book.apply(new_cmd(2, 1, Side::Sell, Kind::Limit, 65535, 1));
    assert!(at_max.iter().any(|e| matches!(e, Event::Accepted { .. })), "{at_max:?}");

    let below_min = book.apply(new_cmd(3, 1, Side::Buy, Kind::Limit, -1, 1));
    assert_eq!(rejected_reason(&below_min, 3), Some(RejectReason::PriceOutOfBand), "{below_min:?}");

    let above_max = book.apply(new_cmd(4, 1, Side::Sell, Kind::Limit, 65536, 1));
    assert_eq!(rejected_reason(&above_max, 4), Some(RejectReason::PriceOutOfBand), "{above_max:?}");

    assert_no_violations(&book);
}

/// Task 4: RefBook::clone() must be total (every field, not just the
/// price maps) and produce a genuinely independent copy, not an aliased
/// one. A partial or shallow clone would give the FOK dry run's shadow
/// book a different account/liveness picture than the real book -- A4
/// reappearing inside the mechanism built to prevent A4.
#[test]
fn clone_is_total_and_independent() {
    let mut book = RefBook::new(wide_cfg());
    book.apply(new_cmd(1, 1, Side::Buy, Kind::Limit, 100, 10));
    book.apply(new_cmd(2, 2, Side::Sell, Kind::Limit, 105, 10));

    let clone = book.clone();
    assert_eq!(clone.digest(), book.digest(), "a fresh clone must match the original bit-for-bit");
    assert!(clone.is_live(1) && clone.is_live(2));

    // Mutate the original; the clone must not observe it (proves this is
    // a deep, independent copy, not a shared reference to the same maps).
    book.apply(Command::Cancel { id: 1 });
    assert!(!book.is_live(1), "sanity: cancel actually removed it from the original");
    assert!(clone.is_live(1), "clone observed a mutation made to the original after cloning -- not independent");
    assert_ne!(clone.digest(), book.digest());

    // And the reverse: mutating a clone must not affect the original.
    let mut clone2 = book.clone();
    clone2.apply(Command::Cancel { id: 2 });
    assert!(!clone2.is_live(2));
    assert!(book.is_live(2), "mutating a clone leaked back into the original");
}

/// Task 4: amend's `new_qty` is the new REMAINING quantity, compared
/// against current remaining -- not the new total compared against the
/// original qty. Tests both directions of the priority asymmetry against
/// `remaining`, using a partially-filled order so "remaining" and
/// "original qty" actually differ and the two interpretations would
/// disagree if conflated.
#[test]
fn amend_qty_is_against_remaining_not_total() {
    let mut book = RefBook::new(wide_cfg());
    // Order for 100, immediately partially filled 40 by a same-price
    // resting seller elsewhere -- resting id 1 starts at remaining=60.
    book.apply(new_cmd(50, 9, Side::Sell, Kind::Limit, 100, 40));
    book.apply(new_cmd(1, 1, Side::Buy, Kind::Limit, 100, 100)); // fills 40, rests 60
    assert!(book.is_live(1));

    // A second resting order behind it, so we can observe FIFO position.
    book.apply(new_cmd(2, 3, Side::Buy, Kind::Limit, 100, 5));

    // Decrease direction: new_qty=50 is below current remaining (60), so
    // this must keep priority (stay ahead of order 2) even though 50 is
    // also below the *original* qty of 100 either way -- the
    // interesting case is qty increase relative to remaining while still
    // being below the original total.
    let events = book.apply(Command::Amend { id: 1, new_price: 100, new_qty: 50 });
    assert_eq!(events, vec![Event::Amended { id: 1, lost_priority: false }]);

    // Increase direction: new_qty=55 is *above* the current remaining
    // (50) but still far below the order's original qty of 100. Under
    // the "compare to remaining" rule this must lose priority. Under a
    // (wrong) "compare to original total" rule it would keep priority,
    // since 55 < 100.
    let events = book.apply(Command::Amend { id: 1, new_price: 100, new_qty: 55 });
    assert_eq!(events, vec![Event::Amended { id: 1, lost_priority: true }]);

    // Having lost priority, order 1 (55 left) must now be behind order 2
    // (5 left) in the level. Sweep with a market sell and check fill
    // order: order 2 should fill first.
    let sweep = book.apply(new_cmd(99, 8, Side::Sell, Kind::Market, 0, 6));
    let fill_order: Vec<u64> = sweep
        .iter()
        .filter_map(|e| match e {
            Event::Fill { maker, .. } => Some(*maker),
            _ => None,
        })
        .collect();
    assert_eq!(fill_order, vec![2, 1], "order 1 should have lost priority to order 2: {sweep:?}");

    assert_no_violations(&book);
}
