//! Slotmap-style order arena. Preallocated at `OrderArena::new`, never
//! grows afterward: `alloc`/`free` only move indices between `slots` and
//! `free`, both already reserved to `capacity`. This is what makes the
//! hot path zero-allocation.
//!
//! The generation counter on `Handle` is an assertion against internal
//! corruption, not a client-facing defense. `Cancel`/`Amend` are
//! `OrderId`-keyed and resolve through `Book`'s live index on every call,
//! so no client command can ever present a stale `Handle` -- only an
//! internal bug in level-unlinking or touch maintenance could. Keep it
//! anyway: fault injection (constructing a dangling handle directly,
//! bypassing the command API) is the only test that can observe its
//! removal, and that test exists in `tests/`.

use types::{AccountId, Kind, OrderId, Price, Qty, Seq, Side};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Handle {
    pub index: u32,
    pub generation: u32,
}

/// A resting order plus its intrusive links. Lives inside the arena, not
/// in a `VecDeque`: `prev`/`next` let a level remove any node in O(1)
/// without a scan.
#[derive(Debug, Clone)]
pub struct OrderNode {
    pub id: OrderId,
    pub account: AccountId,
    pub side: Side,
    pub kind: Kind,
    pub price: Price,
    pub qty: Qty,
    pub remaining: Qty,
    pub seq: Seq,
    pub prev: Option<Handle>,
    pub next: Option<Handle>,
}

struct Slot {
    generation: u32,
    node: Option<OrderNode>,
}

pub struct OrderArena {
    slots: Vec<Slot>,
    free: Vec<u32>,
    capacity: usize,
}

impl OrderArena {
    pub fn new(capacity: usize) -> Self {
        let slots = (0..capacity).map(|_| Slot { generation: 0, node: None }).collect();
        let free = (0..capacity as u32).rev().collect();
        OrderArena { slots, free, capacity }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn live_count(&self) -> usize {
        self.capacity - self.free.len()
    }

    pub fn has_free_slot(&self) -> bool {
        !self.free.is_empty()
    }

    /// Counts slots actually holding a node, by walking every slot,
    /// rather than deriving it from `capacity - free.len()`. Used only by
    /// I8's invariant check, which exists precisely to catch the case
    /// where those two numbers have diverged (a leak or a double-free),
    /// so it must not share the arithmetic it is checking.
    pub fn occupied_count(&self) -> usize {
        self.slots.iter().filter(|s| s.node.is_some()).count()
    }

    /// Panics if called without checking `has_free_slot` first: that
    /// check is `Config::capacity` enforcement's job, at New ingress,
    /// before this is ever reached.
    pub fn alloc(&mut self, node: OrderNode) -> Handle {
        let index = self.free.pop().expect("alloc called with no free slot; capacity should have been checked at ingress");
        let slot = &mut self.slots[index as usize];
        slot.node = Some(node);
        Handle { index, generation: slot.generation }
    }

    /// Frees `handle`'s slot and bumps its generation, so any handle
    /// still holding the old generation is provably stale from this
    /// point on. Returns the freed node.
    pub fn free(&mut self, handle: Handle) -> OrderNode {
        let slot = &mut self.slots[handle.index as usize];
        assert_eq!(slot.generation, handle.generation, "stale handle freed: generation mismatch (internal corruption)");
        let node = slot.node.take().expect("handle points to an already-free slot (internal corruption)");
        slot.generation = slot.generation.wrapping_add(1);
        self.free.push(handle.index);
        node
    }

    pub fn get(&self, handle: Handle) -> &OrderNode {
        let slot = &self.slots[handle.index as usize];
        assert_eq!(slot.generation, handle.generation, "stale handle read: generation mismatch (internal corruption)");
        slot.node.as_ref().expect("handle points to an unoccupied slot (internal corruption)")
    }

    pub fn get_mut(&mut self, handle: Handle) -> &mut OrderNode {
        let slot = &mut self.slots[handle.index as usize];
        assert_eq!(slot.generation, handle.generation, "stale handle write: generation mismatch (internal corruption)");
        slot.node.as_mut().expect("handle points to an unoccupied slot (internal corruption)")
    }
}
