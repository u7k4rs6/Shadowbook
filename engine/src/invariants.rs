//! The nine invariants, checked against the optimized engine. Mirrors
//! `reference::invariants` in shape, but I8 (arena accounting) is real
//! here -- `reference` has no arena to account for -- and I5/I7 gain
//! extra structural checks specific to the bitmap/arena representation
//! that have no analogue in a `BTreeMap`-backed book.
//!
//! I2, I3, I4 are event-stream properties, not book-snapshot properties,
//! and are checked the same way for both engines: by `types::EventAuditor`
//! consuming the emitted events, not by this module.

use std::collections::HashSet;

use types::Kind;

use crate::Book;

pub fn check_invariants(book: &Book) -> Vec<String> {
    let mut violations = Vec::new();

    check_i1_no_cross(book, &mut violations);
    check_i5_bitmap_matches_occupancy(book, &mut violations);
    check_i6_time_priority(book, &mut violations);
    check_i7_live_set_integrity(book, &mut violations);
    check_i8_arena_accounting(book, &mut violations);
    check_only_resting_kinds(book, &mut violations);

    violations
}

/// I1. The book never crosses: `best_bid < best_ask`, strictly.
fn check_i1_no_cross(book: &Book, out: &mut Vec<String>) {
    if let (Some(bid), Some(ask)) = (book.best_bid(), book.best_ask()) {
        if bid >= ask {
            out.push(format!("I1: book crosses, best_bid={bid} best_ask={ask}"));
        }
    }
}

/// I5. Price priority holds. Unlike `reference`'s `BTreeMap`, the
/// engine's "which tick is best" answer is derived fresh from the
/// summary bitmap on every call, so there is no separate cached pointer
/// that could desync from it -- the actual risk here is the bitmap bit
/// itself disagreeing with whether a level is really occupied (a stale
/// bit left over from an incomplete level-empty transition, the exact
/// shape A9 targets). Checked directly against every tick, not inferred
/// from `best_bid`/`best_ask`.
fn check_i5_bitmap_matches_occupancy(book: &Book, out: &mut Vec<String>) {
    for (name, levels) in [("bid", &book.bids), ("ask", &book.asks)] {
        for tick in 0..levels.n_ticks() {
            let bit = levels.bit_set(tick);
            let occupied = !levels.is_empty(tick);
            if bit != occupied {
                out.push(format!("I5: {name} tick {tick}: summary bit={bit} but level occupied={occupied}"));
            }
        }
    }
}

/// I6. Time priority holds within a level: sequence numbers strictly
/// increase walking a level from head (front, fills first) to tail.
fn check_i6_time_priority(book: &Book, out: &mut Vec<String>) {
    for (name, levels) in [("bid", &book.bids), ("ask", &book.asks)] {
        for tick in 0..levels.n_ticks() {
            let mut prev_seq = None;
            let mut cur = levels.head(tick);
            while let Some(h) = cur {
                let node = book.arena.get(h);
                if let Some(prev) = prev_seq {
                    if node.seq <= prev {
                        out.push(format!(
                            "I6: {name} level at tick {tick} not FIFO by seq: order {} (seq {}) follows seq {prev}",
                            node.id, node.seq
                        ));
                    }
                }
                prev_seq = Some(node.seq);
                cur = node.next;
            }
        }
    }
}

/// I7. Live set integrity, checked bidirectionally: every order reachable
/// by walking every level appears exactly once, and consistently, in the
/// live index; every live-index entry is reachable from a level. Neither
/// direction alone catches both the cancel-racing-a-fill shape (A2) and a
/// leaked slot shape (an order removed from a level but left in `live`,
/// or vice versa).
fn check_i7_live_set_integrity(book: &Book, out: &mut Vec<String>) {
    let mut walked = HashSet::new();

    for (side_name, levels) in [("bid", &book.bids), ("ask", &book.asks)] {
        for tick in 0..levels.n_ticks() {
            let mut cur = levels.head(tick);
            while let Some(h) = cur {
                let node = book.arena.get(h);
                if !walked.insert(node.id) {
                    out.push(format!("I7: order {} appears twice while walking the {side_name} book", node.id));
                }
                match book.live.get(&node.id) {
                    None => out.push(format!("I7: order {} is in the {side_name} book but not in the live index", node.id)),
                    Some(&recorded_handle) => {
                        if recorded_handle != h {
                            out.push(format!(
                                "I7: order {} lives on the {side_name} book at a handle the live index disagrees with",
                                node.id
                            ));
                        }
                    }
                }
                cur = node.next;
            }
        }
    }

    for id in book.live.keys() {
        if !walked.contains(id) {
            out.push(format!("I7: order {id} is in the live index but not reachable by walking any level"));
        }
    }
}

/// I8. Arena accounting: `live_orders + free_slots == capacity`, and,
/// more pointedly, the id-keyed live index and the arena's own occupied-
/// slot count must agree. `OrderArena::occupied_count` walks every slot
/// rather than deriving from `capacity - free.len()`, so this actually
/// catches a leak or a double-free instead of re-deriving the same
/// arithmetic it is meant to check.
fn check_i8_arena_accounting(book: &Book, out: &mut Vec<String>) {
    let capacity = book.arena.capacity();
    let occupied = book.arena.occupied_count();
    let live_count = book.arena.live_count();

    if occupied != live_count {
        out.push(format!(
            "I8: arena's own occupied-slot walk ({occupied}) disagrees with its live_count() bookkeeping ({live_count})"
        ));
    }
    if occupied > capacity {
        out.push(format!("I8: occupied slot count ({occupied}) exceeds arena capacity ({capacity})"));
    }
    if book.live.len() != occupied {
        out.push(format!(
            "I8: live index size ({}) disagrees with the arena's occupied-slot count ({occupied})",
            book.live.len()
        ));
    }
}

/// Not one of the numbered nine, but load-bearing for the post-only +
/// amend interaction, exactly as in `reference`: `Market`, `Ioc`, and
/// `Fok` never rest by definition, so one found resting means something
/// upstream let a transient order become persistent.
fn check_only_resting_kinds(book: &Book, out: &mut Vec<String>) {
    for levels in [&book.bids, &book.asks] {
        for tick in 0..levels.n_ticks() {
            let mut cur = levels.head(tick);
            while let Some(h) = cur {
                let node = book.arena.get(h);
                if matches!(node.kind, Kind::Market | Kind::Ioc | Kind::Fok) {
                    out.push(format!("order {} of kind {:?} is resting, but that kind never rests", node.id, node.kind));
                }
                cur = node.next;
            }
        }
    }
}
