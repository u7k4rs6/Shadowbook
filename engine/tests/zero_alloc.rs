//! Enforces "no allocation in the optimized engine's hot path" (tested,
//! not assumed) by wrapping the global allocator in a counter. This is
//! its own test binary specifically so `#[global_allocator]` -- settable
//! once per binary -- does not collide with anything else in the
//! workspace.
//!
//! `unsafe impl GlobalAlloc` here does not conflict with `engine`'s own
//! `#![forbid(unsafe_code)]`: that forbid applies to the library crate's
//! own source, not to a separate integration-test binary that merely
//! links against it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use engine::Book;
use types::{Command, Config, Kind, Side};

struct CountingAllocator;

static ALLOC_COUNT: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOC_COUNT.fetch_add(1, Ordering::SeqCst);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOC_COUNT.fetch_add(1, Ordering::SeqCst);
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOC_COUNT.fetch_add(1, Ordering::SeqCst);
        unsafe { System.alloc_zeroed(layout) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

/// Ten million hot-path commands (five million New/Cancel pairs at a
/// single touch level, strictly increasing ids so every New is a genuine
/// accept-then-rest, not a reject) after construction, which is allowed
/// to allocate (arena, level arrays, event ring, live index, and
/// retirement ring are all reserved up front) and is excluded from the
/// measured window by taking the allocation-count baseline after `Book::
/// new` returns.
#[test]
fn zero_allocations_across_ten_million_hot_path_commands() {
    let cfg = Config { n_ticks: 4096, capacity: 4096, id_retirement_window: 16_384 };
    let mut book = Book::new(cfg);

    // One warm-up New/Cancel cycle before the measured window: Rust's
    // `HashMap` seeds its `RandomState` lazily, on first real hashing
    // operation, not at construction. Without this, the baseline below
    // would be taken before that one-time thread-local init and the
    // first iteration of the real loop would wrongly look like an
    // engine allocation.
    book.apply(Command::New { id: u64::MAX, account: 1, side: Side::Buy, kind: Kind::Limit, price: 100, qty: 1 });
    book.apply(Command::Cancel { id: u64::MAX });

    let baseline = ALLOC_COUNT.load(Ordering::SeqCst);

    for id in 1u64..=5_000_000 {
        book.apply(Command::New { id, account: 1, side: Side::Buy, kind: Kind::Limit, price: 100, qty: 1 });
        book.apply(Command::Cancel { id });
    }

    let after = ALLOC_COUNT.load(Ordering::SeqCst);
    assert_eq!(after, baseline, "engine allocated {} time(s) across the hot-path burst", after - baseline);
    assert_eq!(book.live_count(), 0);
}
