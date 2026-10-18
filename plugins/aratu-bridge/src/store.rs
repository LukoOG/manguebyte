//! The events waiting for their beat, on the audio thread: one store for
//! the current lap of the timeline and one for the next, each a heap of
//! fixed capacity ordered by beat and, on the same beat, by arrival.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use manguebyte_web::EventRecord;

/// An event waiting for its beat.
#[derive(Debug, Clone, Copy)]
pub struct Waiting {
    /// Its beat on the host's timeline.
    pub beat: f64,
    /// When it arrived, among every event: ties on a beat keep this order.
    pub seq: u64,
    /// The record; its frame is set when it is pushed into the engine.
    pub record: EventRecord,
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
    /// Reversed, so that the heap's top is the earliest event.
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .beat
            .total_cmp(&self.beat)
            .then(other.seq.cmp(&self.seq))
    }
}

/// Events by beat, at most a fixed number. Never allocates once made.
#[derive(Debug)]
pub struct Store {
    heap: BinaryHeap<Waiting>,
    capacity: usize,
}

impl Store {
    /// A store for `capacity` events. Allocates.
    pub fn new(capacity: usize) -> Self {
        Self {
            heap: BinaryHeap::with_capacity(capacity),
            capacity,
        }
    }

    /// Adds an event; `false`, dropping it, if the store is full.
    pub fn insert(&mut self, waiting: Waiting) -> bool {
        if self.heap.len() >= self.capacity {
            return false;
        }
        self.heap.push(waiting);
        true
    }

    /// The earliest event.
    pub fn peek(&self) -> Option<&Waiting> {
        self.heap.peek()
    }

    /// Takes the earliest event.
    pub fn pop(&mut self) -> Option<Waiting> {
        self.heap.pop()
    }

    /// How many events wait.
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    /// Whether none waits.
    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    /// Drops every event; returns how many there were.
    pub fn clear(&mut self) -> usize {
        let count = self.heap.len();
        self.heap.clear();
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_come_out_by_beat_then_by_arrival() {
        let mut store = Store::new(8);
        for (seq, beat) in [3.0, 1.0, 2.0, 1.0, -0.5].into_iter().enumerate() {
            let record = EventRecord::note_on(0, seq as u16, 60, 1.0);
            assert!(store.insert(Waiting {
                beat,
                seq: seq as u64,
                record
            }));
        }
        let order: Vec<u16> = std::iter::from_fn(|| store.pop())
            .map(|waiting| waiting.record.part)
            .collect();
        assert_eq!(order, [4, 1, 3, 2, 0]);
    }

    #[test]
    fn a_full_store_refuses_and_keeps_its_capacity() {
        let mut store = Store::new(2);
        let waiting = |beat| Waiting {
            beat,
            seq: 0,
            record: EventRecord::default(),
        };
        assert!(store.insert(waiting(1.0)));
        assert!(store.insert(waiting(2.0)));
        assert!(!store.insert(waiting(0.0)));
        assert_eq!(store.clear(), 2);
        assert!(store.is_empty());
        assert!(store.heap.capacity() >= 2);
    }
}
