//! A6, non-differential: `qty = u64::MAX` at the highest permitted tick
//! must not overflow a notional computation in EITHER engine. This
//! cannot be caught by the differential harness at all -- both engines
//! widen (or fail to widen) identically, so if both got the widening
//! wrong, event-stream comparison would still pass while both are wrong.
//! Oracle agreement is not correctness when both oracles share the bug;
//! this is the standing proof, and it needs its own dedicated,
//! non-differential assertion, checked against each engine's own
//! `notional` function directly.

use fuzz::FUZZ_CONFIG;

#[test]
fn notional_overflow_widens_to_i128_in_both_engines() {
    let price = FUZZ_CONFIG.tick_max();
    let qty = u64::MAX;
    let expected = (price as i128) * (qty as i128);

    let ref_notional = reference::notional(price, qty);
    let engine_notional = engine::notional(price, qty);

    assert_eq!(ref_notional, expected, "reference::notional truncated instead of widening");
    assert_eq!(engine_notional, expected, "engine::notional truncated instead of widening");
    assert!(ref_notional > i64::MAX as i128, "reference::notional should exceed i64::MAX, proving no truncation");
    assert!(engine_notional > i64::MAX as i128, "engine::notional should exceed i64::MAX, proving no truncation");
}
