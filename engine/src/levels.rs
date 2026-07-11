//! Dense tick array plus a `u64` bitmap summary, one bit per tick. Finding
//! the next occupied level is a `trailing_zeros`/`leading_zeros` scan
//! across at most `n_ticks / 64` words, not a walk tick by tick, so a
//! sparse book still resolves the touch in O(1) in practice (liquidity
//! clusters near it) and O(words) worst case, never O(n_ticks).
//!
//! One `Levels` per side (bids, asks): each is independently indexed by
//! absolute tick, `TickIdx = tick as usize`, no offset arithmetic, so the
//! low-boundary edge (a `usize` underflow near tick zero) stays directly
//! reachable rather than hidden behind a subtraction that would be
//! correct by construction.
//!
//! This module only holds level-local bookkeeping (head/tail/count/bit).
//! It knows nothing about the arena; the intrusive-list push/unlink logic
//! that also needs to touch `OrderNode.prev`/`next` lives in `book.rs`,
//! which borrows a `Levels` and an `OrderArena` as sibling fields rather
//! than one owning the other, sidestepping any aliasing problem.

use crate::arena::Handle;

#[derive(Debug, Clone, Copy, Default)]
struct Level {
    head: Option<Handle>,
    tail: Option<Handle>,
    count: u32,
}

pub struct Levels {
    levels: Vec<Level>,
    summary: Vec<u64>,
    n_ticks: usize,
}

impl Levels {
    fn word_count(n_ticks: usize) -> usize {
        n_ticks.div_ceil(64)
    }

    pub fn new(n_ticks: usize) -> Self {
        Levels { levels: vec![Level::default(); n_ticks], summary: vec![0u64; Self::word_count(n_ticks)], n_ticks }
    }

    pub fn head(&self, tick: usize) -> Option<Handle> {
        self.levels[tick].head
    }

    pub fn tail(&self, tick: usize) -> Option<Handle> {
        self.levels[tick].tail
    }

    pub fn is_empty(&self, tick: usize) -> bool {
        self.levels[tick].count == 0
    }

    pub fn n_ticks(&self) -> usize {
        self.n_ticks
    }

    /// Raw summary bit for `tick`, independent of `is_empty`/`count`, for
    /// invariant checks that want to verify the bitmap itself agrees with
    /// actual level occupancy rather than trusting the same bookkeeping
    /// that produced both.
    pub fn bit_set(&self, tick: usize) -> bool {
        (self.summary[tick / 64] >> (tick % 64)) & 1 == 1
    }

    pub fn set_head(&mut self, tick: usize, h: Option<Handle>) {
        self.levels[tick].head = h;
    }

    pub fn set_tail(&mut self, tick: usize, h: Option<Handle>) {
        self.levels[tick].tail = h;
    }

    /// Bumps the level's count and, if this is the level's first order,
    /// sets its summary bit.
    pub fn inc_count(&mut self, tick: usize) {
        let level = &mut self.levels[tick];
        if level.count == 0 {
            self.summary[tick / 64] |= 1u64 << (tick % 64);
        }
        self.levels[tick].count += 1;
    }

    /// Drops the level's count and, if it just emptied, clears its
    /// summary bit. This single clear-on-empty site is what A9 depends
    /// on: there is no other place a bit is ever cleared, so a stale bit
    /// surviving an empty level is structurally impossible here.
    pub fn dec_count(&mut self, tick: usize) {
        let level = &mut self.levels[tick];
        level.count -= 1;
        if level.count == 0 {
            self.summary[tick / 64] &= !(1u64 << (tick % 64));
        }
    }

    /// Lowest occupied tick, or `None` if the side is empty. The bounded
    /// scan (`self.summary.len()` words, never further) is what keeps A9
    /// (full sweep of an empty book) from walking off the end of the
    /// summary array.
    pub fn lowest_occupied(&self) -> Option<usize> {
        self.lowest_occupied_from(0)
    }

    /// Highest occupied tick, or `None` if the side is empty.
    pub fn highest_occupied(&self) -> Option<usize> {
        if self.n_ticks == 0 {
            return None;
        }
        self.highest_occupied_upto(self.n_ticks - 1)
    }

    /// Lowest occupied tick `>= start_tick`, or `None`. Used by the FOK
    /// dry-run counting pass, which cannot mutate the level (no
    /// self-match cancellations actually happen), so it cannot rely on
    /// "the current best just disappeared" to advance -- it must ask for
    /// strictly the next one explicitly.
    pub fn lowest_occupied_from(&self, start_tick: usize) -> Option<usize> {
        if start_tick >= self.n_ticks {
            return None;
        }
        let start_word = start_tick / 64;
        let start_bit = start_tick % 64;
        let masked = self.summary[start_word] & (!0u64 << start_bit);
        if masked != 0 {
            return Some(start_word * 64 + masked.trailing_zeros() as usize);
        }
        for (wi, &word) in self.summary.iter().enumerate().skip(start_word + 1) {
            if word != 0 {
                return Some(wi * 64 + word.trailing_zeros() as usize);
            }
        }
        None
    }

    /// Highest occupied tick `<= end_tick`, or `None`.
    pub fn highest_occupied_upto(&self, end_tick: usize) -> Option<usize> {
        if self.n_ticks == 0 {
            return None;
        }
        let end_tick = end_tick.min(self.n_ticks - 1);
        let end_word = end_tick / 64;
        let end_bit = end_tick % 64;
        // Mask off bits above end_bit. Shifting by 64 is UB, so guard the
        // full-word case explicitly.
        let mask = if end_bit == 63 { !0u64 } else { (1u64 << (end_bit + 1)) - 1 };
        let masked = self.summary[end_word] & mask;
        if masked != 0 {
            return Some(end_word * 64 + (63 - masked.leading_zeros() as usize));
        }
        for wi in (0..end_word).rev() {
            let word = self.summary[wi];
            if word != 0 {
                return Some(wi * 64 + (63 - word.leading_zeros() as usize));
            }
        }
        None
    }

    /// Strictly-greater-than variant of `lowest_occupied_from`, for the
    /// FOK counting pass advancing past a tick it has already tallied.
    pub fn lowest_occupied_after(&self, tick: usize) -> Option<usize> {
        self.lowest_occupied_from(tick + 1)
    }

    /// Strictly-less-than variant of `highest_occupied_upto`.
    pub fn highest_occupied_before(&self, tick: usize) -> Option<usize> {
        if tick == 0 {
            return None;
        }
        self.highest_occupied_upto(tick - 1)
    }
}
