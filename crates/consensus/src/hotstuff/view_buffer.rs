//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::{
    collections::{BTreeMap, VecDeque, btree_map::Entry},
    fmt::Display,
    mem,
};

use log::*;
use tari_ootle_common_types::{Epoch, NodeHeight};

const LOG_TARGET: &str = "tari::ootle::consensus::hotstuff::view_buffer";

/// A consensus view, ordered by epoch then height.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct View {
    pub epoch: Epoch,
    pub height: NodeHeight,
}

impl View {
    pub fn new(epoch: Epoch, height: NodeHeight) -> Self {
        Self { epoch, height }
    }
}

impl Display for View {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.epoch, self.height)
    }
}

/// A buffer of items belonging to views we have not reached yet, keyed by view and FIFO within a view.
///
/// `capacity` bounds the items held across every view at once: the view an item names is chosen by its
/// sender, so the number of views in play is whatever our peers make it, and the total is the only quantity
/// that can be held to a limit.
///
/// At capacity, the item furthest in the future is evicted to make room for a nearer one, and an item that is
/// itself the furthest is refused. Consensus consumes views in order, so the nearest views are the ones it is
/// about to need and a flood aimed at distant views cannot crowd them out.
pub struct ViewBuffer<T> {
    buffer: BTreeMap<View, VecDeque<T>>,
    len: usize,
    capacity: usize,
}

impl<T> ViewBuffer<T> {
    /// Creates a buffer holding at most `capacity` items in total.
    ///
    /// # Panics
    /// Panics if `capacity` is zero.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "ViewBuffer capacity must be non-zero");
        Self {
            buffer: BTreeMap::new(),
            len: 0,
            capacity,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Buffers `item` for `view`. Returns `Err(item)` if the buffer is full of items for views at or nearer
    /// than `view`.
    pub fn insert(&mut self, view: View, item: T) -> Result<(), T> {
        while self.len >= self.capacity {
            let Some(mut furthest) = self.buffer.last_entry() else {
                break;
            };
            if *furthest.key() <= view {
                return Err(item);
            }
            let evicted_from = *furthest.key();
            furthest.get_mut().pop_back();
            self.len -= 1;
            if furthest.get().is_empty() {
                furthest.remove();
            }
            warn!(
                target: LOG_TARGET,
                "🗑️ Buffer is at capacity ({}): evicted an item for view {} to make room for view {}",
                self.capacity, evicted_from, view
            );
        }

        self.buffer.entry(view).or_default().push_back(item);
        self.len += 1;
        Ok(())
    }

    /// Removes and returns the oldest item buffered for `view`.
    pub fn pop_front(&mut self, view: &View) -> Option<T> {
        let Entry::Occupied(mut entry) = self.buffer.entry(*view) else {
            return None;
        };
        let item = entry.get_mut().pop_front()?;
        self.len -= 1;
        if entry.get().is_empty() {
            entry.remove();
        }
        Some(item)
    }

    /// Drops every item buffered for a view before `view`, returning the number dropped.
    pub fn discard_before(&mut self, view: View) -> usize {
        let mut discarded = mem::take(&mut self.buffer);
        self.buffer = discarded.split_off(&view);
        let num_discarded = discarded.values().map(VecDeque::len).sum();
        self.len -= num_discarded;
        num_discarded
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.len = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(height: u64) -> View {
        View::new(Epoch(1), NodeHeight(height))
    }

    #[test]
    fn it_pops_items_for_a_view_in_order() {
        let mut buffer = ViewBuffer::new(10);
        buffer.insert(view(1), "a").unwrap();
        buffer.insert(view(2), "b").unwrap();
        buffer.insert(view(1), "c").unwrap();

        assert_eq!(buffer.pop_front(&view(1)), Some("a"));
        assert_eq!(buffer.pop_front(&view(1)), Some("c"));
        assert_eq!(buffer.pop_front(&view(1)), None);
        assert_eq!(buffer.pop_front(&view(2)), Some("b"));
        assert_eq!(buffer.len(), 0);
    }

    #[test]
    fn it_discards_items_before_a_view() {
        let mut buffer = ViewBuffer::new(10);
        buffer.insert(view(1), "a").unwrap();
        buffer.insert(view(1), "b").unwrap();
        buffer.insert(view(2), "c").unwrap();
        buffer.insert(View::new(Epoch(2), NodeHeight(1)), "d").unwrap();

        assert_eq!(buffer.discard_before(view(2)), 2);
        assert_eq!(buffer.len(), 2);
        assert_eq!(buffer.pop_front(&view(1)), None);
        assert_eq!(buffer.pop_front(&view(2)), Some("c"));
    }

    #[test]
    fn a_single_view_cannot_exceed_the_total_capacity() {
        let mut buffer = ViewBuffer::new(3);
        for i in 0..3 {
            buffer.insert(view(1), i).unwrap();
        }
        assert_eq!(buffer.insert(view(1), 3), Err(3));
        assert_eq!(buffer.len(), 3);
    }

    #[test]
    fn many_views_cannot_exceed_the_total_capacity() {
        let mut buffer = ViewBuffer::new(3);
        for height in 1..=100 {
            let _ignore = buffer.insert(view(height), height);
        }
        assert_eq!(buffer.len(), 3);
    }

    #[test]
    fn a_nearer_view_evicts_the_furthest_item() {
        let mut buffer = ViewBuffer::new(2);
        buffer.insert(view(10), "far").unwrap();
        buffer.insert(view(11), "further").unwrap();

        buffer.insert(view(1), "near").unwrap();

        assert_eq!(buffer.len(), 2);
        assert_eq!(buffer.pop_front(&view(1)), Some("near"));
        assert_eq!(buffer.pop_front(&view(10)), Some("far"));
        assert_eq!(buffer.pop_front(&view(11)), None);
    }

    #[test]
    fn an_item_for_the_furthest_view_is_refused() {
        let mut buffer = ViewBuffer::new(2);
        buffer.insert(view(1), "near").unwrap();
        buffer.insert(view(10), "far").unwrap();

        assert_eq!(buffer.insert(view(10), "same"), Err("same"));
        assert_eq!(buffer.insert(view(11), "furthest"), Err("furthest"));
        assert_eq!(buffer.len(), 2);
        assert_eq!(buffer.pop_front(&view(1)), Some("near"));
    }
}
