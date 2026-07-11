//! The nine invariants from Adversarial Test Plan section 3, checked
//! against the reference engine.
//!
//! `check_invariants(&RefBook)` only has access to a single snapshot of
//! the book, so it can only check properties that are true statements
//! about *that* snapshot. Four of the nine invariants are not
//! snapshot-local and are handled elsewhere:
//!
//! - **I2** (quantity conservation) and **I3**/**I4** (fill price/limit
//!   correctness) are properties of the *event stream* produced across a
//!   run, not of a resting book. I3/I4 are additionally guarded with
//!   `debug_assert!` at the point fills are constructed in
//!   `RefBook::run_match`, so a violation panics immediately in debug
//!   builds rather than surviving to be checked later. A full I2 running
//!   total belongs in the differential fuzz harness (Day 3), which is the
//!   first place multiple commands and their events are accumulated
//!   across a run.
//! - **I8** (arena accounting) is meaningless here: the reference engine
//!   has no arena, no free list, no slot reuse. It applies to `engine/`
//!   only.
//! - **I9** (replay) needs two book instances (fresh vs. live) and is
//!   exposed as `RefBook::replay_matches`, not folded into this function.
//!
//! I1, I5, I6, and I7 are genuinely snapshot properties and are checked
//! here in full.

use std::collections::HashSet;

use crate::RefBook;

pub fn check_invariants(book: &RefBook) -> Vec<String> {
    let mut violations = Vec::new();

    check_i1_no_cross(book, &mut violations);
    check_i5_price_priority(book, &mut violations);
    check_i6_time_priority(book, &mut violations);
    check_i7_live_set_integrity(book, &mut violations);
    check_only_resting_kinds(book, &mut violations);

    violations
}

/// Not one of the numbered nine, but load-bearing for the post-only +
/// amend interaction: `Market`, `Ioc`, and `Fok` orders never rest by
/// definition (section 6), so if one is ever found resting in the book,
/// something upstream let a transient order become persistent -- most
/// likely a matching bug that left `remaining > 0` for a kind that
/// should have discarded its remainder instead.
fn check_only_resting_kinds(book: &RefBook, out: &mut Vec<String>) {
    use types::Kind;

    for level in book.bids.values().chain(book.asks.values()) {
        for o in level {
            if matches!(o.kind, Kind::Market | Kind::Ioc | Kind::Fok) {
                out.push(format!("order {} of kind {:?} is resting, but that kind never rests", o.id, o.kind));
            }
        }
    }
}

/// I1. The book never crosses: `best_bid < best_ask`, strictly.
fn check_i1_no_cross(book: &RefBook, out: &mut Vec<String>) {
    if let (Some(bid), Some(ask)) = (book.best_bid(), book.best_ask()) {
        if bid >= ask {
            out.push(format!("I1: book crosses, best_bid={bid} best_ask={ask}"));
        }
    }
}

/// I5. Price priority holds: the map's own ordering guarantees `best_bid`
/// is the true best (highest) bid and `best_ask` the true best (lowest)
/// ask, and every other resting level is worse. `BTreeMap` makes this
/// true by construction, but we assert it anyway rather than trust the
/// data structure blindly; a reference engine should not take anything
/// on faith, including its own invariants.
fn check_i5_price_priority(book: &RefBook, out: &mut Vec<String>) {
    if let Some(best) = book.best_bid() {
        for (std::cmp::Reverse(price), level) in book.bids.iter() {
            if !level.is_empty() && *price > best {
                out.push(format!("I5: bid level at {price} is better than reported best_bid {best}"));
            }
        }
    }
    if let Some(best) = book.best_ask() {
        for (price, level) in book.asks.iter() {
            if !level.is_empty() && *price < best {
                out.push(format!("I5: ask level at {price} is better than reported best_ask {best}"));
            }
        }
    }
}

/// I6. Time priority holds within a level: sequence numbers strictly
/// increase walking a level from head (front, fills first) to tail.
fn check_i6_time_priority(book: &RefBook, out: &mut Vec<String>) {
    for (std::cmp::Reverse(price), level) in book.bids.iter() {
        check_level_seq_increasing(level, price, "bid", out);
    }
    for (price, level) in book.asks.iter() {
        check_level_seq_increasing(level, price, "ask", out);
    }
}

fn check_level_seq_increasing(
    level: &std::collections::VecDeque<crate::Order>,
    price: &i64,
    side: &str,
    out: &mut Vec<String>,
) {
    let mut prev_seq = None;
    for o in level {
        if let Some(prev) = prev_seq {
            if o.seq <= prev {
                out.push(format!(
                    "I6: {side} level at {price} not FIFO by seq: order {} (seq {}) follows seq {prev}",
                    o.id, o.seq
                ));
            }
        }
        prev_seq = Some(o.seq);
    }
}

/// I7. Live set integrity: every order reachable by walking every level
/// appears exactly once in the live index, and vice versa.
fn check_i7_live_set_integrity(book: &RefBook, out: &mut Vec<String>) {
    use types::Side;

    let mut walked = HashSet::new();

    for (side_name, expected_side, level) in book
        .bids
        .values()
        .map(|l| ("bid", Side::Buy, l))
        .chain(book.asks.values().map(|l| ("ask", Side::Sell, l)))
    {
        for o in level {
            if !walked.insert(o.id) {
                out.push(format!("I7: order {} appears twice while walking the {side_name} book", o.id));
            }
            match book.live.get(&o.id) {
                None => out.push(format!("I7: order {} is in the {side_name} book but not in the live index", o.id)),
                Some(&recorded_side) => {
                    if recorded_side != expected_side {
                        out.push(format!(
                            "I7: order {} lives on the {side_name} book but live index says {:?}",
                            o.id, recorded_side
                        ));
                    }
                }
            }
        }
    }

    for id in book.live.keys() {
        if !walked.contains(id) {
            out.push(format!("I7: order {id} is in the live index but not reachable by walking any level"));
        }
    }
}
