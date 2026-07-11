//! `fuzz`: the differential fuzzer.
//!
//! Not built yet. Per the Session 1 plan, this crate is a placeholder so
//! the workspace has the shape described in PRD section 8. Day 3 work:
//! wire `reference` and `engine` to the same command stream via
//! `proptest`/`cargo-fuzz`, assert identical events after every command,
//! check invariants, and check replay at the end of each case.
