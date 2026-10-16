//! Events that have arrived and wait for their frame.

use core::cmp::Ordering;

use crate::record::Event;

/// One waiting event. Ordered by frame, then by arrival, so events on the
/// same frame are applied in the order they were pushed.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Waiting {
    pub(crate) frame: u64,
    pub(crate) sequence: u64,
    pub(crate) event: Event,
}

impl PartialEq for Waiting {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Waiting {}

impl PartialOrd for Waiting {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Waiting {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.frame, self.sequence).cmp(&(other.frame, other.sequence))
    }
}

/// What [`Pending::push`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Admitted {
    /// The event waits; nothing else changed.
    Queued,
    /// The queue was full: the event waits, and the event that was due last
    /// was dropped to make room for it.
    Displaced,
    /// The queue was full and the event is due no earlier than every event
    /// waiting: it was not taken.
    Refused,
}

/// A double-ended priority queue of fixed capacity: a min-max heap
/// (M. D. Atkinson, J.-R. Sack, N. Santoro and T. Strothotte, "Min-max heaps
/// and generalized priority queues", Communications of the ACM 29(10),
/// 1986). The earliest and the latest event can both be taken out in
/// logarithmic time, so a full queue can make room for an earlier event by
/// dropping its latest one. It never grows, so nothing it does allocates.
///
/// Levels alternate: every element on an even level (the root's) is no later
/// than everything below it, and every element on an odd level is no
/// earlier than everything below it.
#[derive(Debug)]
pub(crate) struct Pending {
    heap: Vec<Waiting>,
    capacity: usize,
    next_sequence: u64,
}

/// Whether index `index` is on a level of minima.
fn on_min_level(index: usize) -> bool {
    // The level of `index` is floor(log2(index + 1)).
    (usize::BITS - 1 - (index + 1).leading_zeros()) % 2 == 0
}

fn parent(index: usize) -> usize {
    (index - 1) / 2
}

impl Pending {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            heap: Vec::with_capacity(capacity),
            capacity,
            next_sequence: 0,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.heap.len()
    }

    pub(crate) fn room(&self) -> usize {
        self.capacity - self.heap.len()
    }

    /// The index of the latest event, if there is one.
    fn latest_index(&self) -> Option<usize> {
        match self.heap.len() {
            0 => None,
            1 => Some(0),
            2 => Some(1),
            _ => Some(if self.heap[1] >= self.heap[2] { 1 } else { 2 }),
        }
    }

    /// Whether an event for `frame` pushed now would be taken: there is
    /// room, or it is due before the latest event waiting.
    pub(crate) fn admits(&self, frame: u64) -> bool {
        // A new event comes after every waiting one on the same frame, so it
        // is earlier than the latest only if its frame is.
        self.room() > 0
            || self
                .latest_index()
                .is_some_and(|latest| frame < self.heap[latest].frame)
    }

    /// Adds an event. A full queue drops its latest event to make room for
    /// an earlier one, and refuses one that is not earlier.
    pub(crate) fn push(&mut self, frame: u64, event: Event) -> Admitted {
        if !self.admits(frame) {
            return Admitted::Refused;
        }
        let outcome = if self.room() == 0 {
            self.pop_latest();
            Admitted::Displaced
        } else {
            Admitted::Queued
        };
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        // Never beyond the capacity reserved at construction: no allocation.
        self.heap.push(Waiting {
            frame,
            sequence,
            event,
        });
        self.bubble_up(self.heap.len() - 1);
        outcome
    }

    /// Takes the earliest event if its frame is before `end`.
    pub(crate) fn pop_before(&mut self, end: u64) -> Option<Waiting> {
        if self.heap.first()?.frame >= end {
            return None;
        }
        let last = self.heap.len() - 1;
        self.heap.swap(0, last);
        let earliest = self.heap.pop();
        if !self.heap.is_empty() {
            self.trickle_down(0);
        }
        earliest
    }

    /// Takes the event due last out.
    fn pop_latest(&mut self) -> Option<Waiting> {
        let index = self.latest_index()?;
        let last = self.heap.len() - 1;
        self.heap.swap(index, last);
        let latest = self.heap.pop();
        if index < self.heap.len() {
            self.trickle_down(index);
        }
        latest
    }

    pub(crate) fn clear(&mut self) {
        self.heap.clear();
    }

    fn bubble_up(&mut self, index: usize) {
        if index == 0 {
            return;
        }
        let up = parent(index);
        if on_min_level(index) {
            if self.heap[index] > self.heap[up] {
                self.heap.swap(index, up);
                self.bubble_up_by(up, Ordering::Greater);
            } else {
                self.bubble_up_by(index, Ordering::Less);
            }
        } else if self.heap[index] < self.heap[up] {
            self.heap.swap(index, up);
            self.bubble_up_by(up, Ordering::Less);
        } else {
            self.bubble_up_by(index, Ordering::Greater);
        }
    }

    /// Moves `index` up its own kind of level, grandparent by grandparent,
    /// while it compares to its grandparent as `toward`.
    fn bubble_up_by(&mut self, mut index: usize, toward: Ordering) {
        while index >= 3 {
            let grandparent = parent(parent(index));
            if self.heap[index].cmp(&self.heap[grandparent]) != toward {
                break;
            }
            self.heap.swap(index, grandparent);
            index = grandparent;
        }
    }

    fn trickle_down(&mut self, index: usize) {
        if on_min_level(index) {
            self.trickle_down_by(index, Ordering::Less);
        } else {
            self.trickle_down_by(index, Ordering::Greater);
        }
    }

    /// Restores the order below `index`, on a level where every element
    /// compares to those below it as `toward` (`Less` on a level of minima).
    fn trickle_down_by(&mut self, mut index: usize, toward: Ordering) {
        let len = self.heap.len();
        loop {
            let first_child = 2 * index + 1;
            if first_child >= len {
                return;
            }
            // The most extreme of the children and grandchildren.
            let mut best = first_child;
            let candidates = [
                first_child + 1,
                4 * index + 3,
                4 * index + 4,
                4 * index + 5,
                4 * index + 6,
            ];
            for candidate in candidates {
                if candidate < len && self.heap[candidate].cmp(&self.heap[best]) == toward {
                    best = candidate;
                }
            }
            if self.heap[best].cmp(&self.heap[index]) != toward {
                return;
            }
            self.heap.swap(best, index);
            if best <= first_child + 1 {
                // A child: it is on the other kind of level, and nothing
                // below it can be out of order.
                return;
            }
            // A grandchild: the element moved down may now be on the wrong
            // side of its new parent.
            let up = parent(best);
            if self.heap[best].cmp(&self.heap[up]) == toward.reverse() {
                self.heap.swap(best, up);
            }
            index = best;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(part: u16) -> Event {
        Event::Choke { part }
    }

    /// Checks the min-max heap property everywhere.
    fn check(pending: &Pending) {
        let heap = &pending.heap;
        for index in 1..heap.len() {
            let mut ancestor = index;
            while ancestor > 0 {
                ancestor = parent(ancestor);
                if on_min_level(ancestor) {
                    assert!(heap[ancestor] <= heap[index], "{index} below {ancestor}");
                } else {
                    assert!(heap[ancestor] >= heap[index], "{index} below {ancestor}");
                }
            }
        }
    }

    #[test]
    fn levels_alternate() {
        let levels: Vec<bool> = (0..16).map(on_min_level).collect();
        let mut expected = vec![true];
        expected.extend([false; 2]);
        expected.extend([true; 4]);
        expected.extend([false; 8]);
        expected.push(true);
        assert_eq!(levels, expected);
    }

    /// Random pushes and pops at both ends, against a sorted list.
    #[test]
    fn it_matches_a_sorted_list_under_random_use() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for capacity in [1usize, 2, 3, 7, 16, 61] {
            let mut pending = Pending::with_capacity(capacity);
            let reserved = pending.heap.capacity();
            let mut reference: Vec<(u64, u64)> = Vec::new();
            let mut sequence = 0u64;
            for _ in 0..20_000 {
                let roll = next() % 10;
                if roll < 6 {
                    let frame = next() % 40;
                    let outcome = pending.push(frame, event(0));
                    let full = reference.len() == capacity;
                    let expected = if !full {
                        Admitted::Queued
                    } else if frame < reference.last().unwrap().0 {
                        Admitted::Displaced
                    } else {
                        Admitted::Refused
                    };
                    assert_eq!(outcome, expected);
                    if expected == Admitted::Displaced {
                        reference.pop();
                    }
                    if expected != Admitted::Refused {
                        reference.push((frame, sequence));
                        reference.sort_unstable();
                        sequence += 1;
                    }
                } else if roll < 9 {
                    let end = next() % 45;
                    let got = pending.pop_before(end).map(|w| (w.frame, w.sequence));
                    let expected = reference.first().copied().filter(|&(frame, _)| frame < end);
                    if expected.is_some() {
                        reference.remove(0);
                    }
                    assert_eq!(got, expected);
                } else {
                    let got = pending.pop_latest().map(|w| (w.frame, w.sequence));
                    assert_eq!(got, reference.pop());
                }
                assert_eq!(pending.len(), reference.len());
                assert_eq!(pending.heap.capacity(), reserved, "the heap grew");
                check(&pending);
            }
        }
    }

    #[test]
    fn a_full_queue_keeps_the_earliest_events() {
        let mut pending = Pending::with_capacity(3);
        for frame in [50, 10, 30] {
            assert_eq!(pending.push(frame, event(0)), Admitted::Queued);
        }
        assert!(!pending.admits(50));
        assert!(pending.admits(49));
        assert_eq!(pending.push(50, event(1)), Admitted::Refused);
        assert_eq!(pending.push(20, event(2)), Admitted::Displaced);
        let order: Vec<u64> = core::iter::from_fn(|| pending.pop_before(u64::MAX))
            .map(|waiting| waiting.frame)
            .collect();
        assert_eq!(order, [10, 20, 30]);
    }
}
