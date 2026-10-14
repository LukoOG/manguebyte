//! The audio thread's store of events that are waiting for their moment.
//!
//! Two things are asked of it, both without allocating: hand out the event
//! that is due first, and, when it is full, say which event is due last so
//! that it can make room for one that is due sooner. A binary heap answers
//! only the first question. This is a min-max heap, which answers both in
//! time logarithmic in the number of items.
//!
//! Source: M. D. Atkinson, J.-R. Sack, N. Santoro and T. Strothotte,
//! *Min-Max Heaps and Generalized Priority Queues*, Communications of the
//! ACM 29 (10), 1986. The items are kept in an array that is read as a
//! binary tree, as in an ordinary heap. Levels of the tree alternate: an
//! item on an even level is no greater than anything below it, and an item
//! on an odd level is no less than anything below it. The least item is
//! therefore the root, and the greatest is one of the root's two children.

/// Something with a place in an order. Two items of one agenda never have
/// the same key.
pub(crate) trait Scheduled {
    /// What items are ordered by.
    type Key: Ord + Copy;

    /// This item's place in the order.
    fn key(&self) -> Self::Key;
}

/// A bounded collection that gives out its first and its last item.
pub(crate) struct Agenda<T> {
    /// Never grows beyond `capacity`, which it was allocated with.
    items: Vec<T>,
    capacity: usize,
}

/// Whether a position in the array is on a level whose items are no greater
/// than what lies below them. The root is.
fn on_min_level(index: usize) -> bool {
    // Level `n` holds positions `2^n - 1` up to `2^(n+1) - 2`.
    (index + 1).ilog2() % 2 == 0
}

fn parent(index: usize) -> Option<usize> {
    index.checked_sub(1).map(|below| below / 2)
}

fn grandparent(index: usize) -> Option<usize> {
    parent(index).and_then(parent)
}

impl<T: Scheduled> Agenda<T> {
    /// An empty agenda with room for `capacity` items. This is the only
    /// place it allocates.
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            items: Vec::with_capacity(capacity),
            capacity,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.items.len()
    }

    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }

    /// The item that comes first in the order.
    pub(crate) fn first(&self) -> Option<&T> {
        self.items.first()
    }

    /// The item that comes last in the order.
    pub(crate) fn last(&self) -> Option<&T> {
        self.items.get(self.last_index()?)
    }

    /// Adds an item, or hands it back if the agenda is full.
    pub(crate) fn push(&mut self, item: T) -> Result<(), T> {
        if self.items.len() >= self.capacity {
            return Err(item);
        }
        // Below the capacity the vector was built with: no allocation.
        self.items.push(item);
        self.sift_up(self.items.len() - 1);
        Ok(())
    }

    /// Takes out the item that comes first in the order.
    pub(crate) fn pop_first(&mut self) -> Option<T> {
        self.take(0)
    }

    /// Takes out the item that comes last in the order.
    pub(crate) fn pop_last(&mut self) -> Option<T> {
        self.take(self.last_index()?)
    }

    /// Where the item that comes last is: the root if it is alone, and
    /// otherwise the greater of the root's children.
    fn last_index(&self) -> Option<usize> {
        match self.items.len() {
            0 => None,
            1 => Some(0),
            2 => Some(1),
            _ => Some(if self.less(1, 2) { 2 } else { 1 }),
        }
    }

    /// Takes out the item at `index`, which is the first or the last in
    /// the order, and puts the tree right again.
    fn take(&mut self, index: usize) -> Option<T> {
        if index >= self.items.len() {
            return None;
        }
        // The item at the end of the array takes the place of the one
        // that leaves, and sinks from there to where it belongs.
        let item = self.items.swap_remove(index);
        self.sift_down(index);
        Some(item)
    }

    /// Whether the item at `a` comes before the item at `b`. False if
    /// either position is empty.
    fn less(&self, a: usize, b: usize) -> bool {
        match (self.items.get(a), self.items.get(b)) {
            (Some(a), Some(b)) => a.key() < b.key(),
            _ => false,
        }
    }

    /// Whether the item at `a` belongs nearer the root than the item at
    /// `b`, on a level of the given kind.
    fn before(&self, a: usize, b: usize, min_level: bool) -> bool {
        if min_level {
            self.less(a, b)
        } else {
            self.less(b, a)
        }
    }

    fn swap(&mut self, a: usize, b: usize) {
        if a < self.items.len() && b < self.items.len() {
            self.items.swap(a, b);
        }
    }

    /// Moves a new item at `index` up to where it belongs.
    fn sift_up(&mut self, mut index: usize) {
        let Some(up) = parent(index) else {
            return;
        };
        let mut min_level = on_min_level(index);
        // An item that is on the wrong side of its parent belongs among
        // the levels of the other kind, and continues from the parent.
        if self.before(up, index, min_level) {
            self.swap(up, index);
            index = up;
            min_level = !min_level;
        }
        while let Some(above) = grandparent(index) {
            if !self.before(index, above, min_level) {
                break;
            }
            self.swap(index, above);
            index = above;
        }
    }

    /// Moves the item at `index` down to where it belongs.
    fn sift_down(&mut self, mut index: usize) {
        let min_level = on_min_level(index);
        loop {
            // The one that belongs nearest the root among the children and
            // grandchildren, which are the next two levels down.
            let first_child = 2 * index + 1;
            let first_grandchild = 2 * first_child + 1;
            let mut best = None;
            for candidate in
                (first_child..first_child + 2).chain(first_grandchild..first_grandchild + 4)
            {
                if candidate >= self.items.len() {
                    break;
                }
                if best.is_none_or(|best| self.before(candidate, best, min_level)) {
                    best = Some(candidate);
                }
            }
            let Some(best) = best else {
                return;
            };
            if !self.before(best, index, min_level) {
                return;
            }
            self.swap(best, index);
            if best < first_grandchild {
                // A child: nothing lies below it on a level of this kind.
                return;
            }
            // A grandchild. What came down may be on the wrong side of
            // the item between the two, which is of the other kind.
            if let Some(between) = parent(best) {
                if self.before(between, best, min_level) {
                    self.swap(between, best);
                }
            }
            index = best;
        }
    }
}

impl<T> Agenda<T> {
    /// Takes out an item, whichever is cheapest to take. For emptying the
    /// agenda: the order of what is left is not kept.
    pub(crate) fn pop_any(&mut self) -> Option<T> {
        self.items.pop()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Scheduled for (i64, u64) {
        type Key = (i64, u64);

        fn key(&self) -> Self::Key {
            *self
        }
    }

    /// A small deterministic generator (xorshift64*).
    struct Random(u64);

    impl Random {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, bound: u64) -> u64 {
            (self.next() >> 33) % bound
        }
    }

    /// Checks the property that defines the structure: every item is in
    /// order with its parent and its grandparent.
    fn assert_well_formed(agenda: &Agenda<(i64, u64)>) {
        for index in 1..agenda.items.len() {
            let item = agenda.items[index];
            let up = agenda.items[parent(index).unwrap()];
            if on_min_level(index) {
                assert!(item < up, "{index}");
            } else {
                assert!(item > up, "{index}");
            }
            if let Some(above) = grandparent(index) {
                let above = agenda.items[above];
                if on_min_level(index) {
                    assert!(item > above, "{index}");
                } else {
                    assert!(item < above, "{index}");
                }
            }
        }
    }

    #[test]
    fn levels_alternate_from_the_root() {
        let kinds: Vec<bool> = (0..15).map(on_min_level).collect();
        assert_eq!(
            kinds,
            [
                true, false, false, true, true, true, true, false, false, false, false, false,
                false, false, false
            ]
        );
        assert_eq!(
            (parent(0), parent(1), parent(2), parent(6)),
            (None, Some(0), Some(0), Some(2))
        );
        assert_eq!(
            (grandparent(2), grandparent(3), grandparent(14)),
            (None, Some(0), Some(2))
        );
    }

    #[test]
    fn it_gives_out_both_ends_like_a_sorted_list() {
        let mut random = Random(0x0A6E_17DA_0A6E_17DA);
        for capacity in [1usize, 2, 3, 4, 5, 7, 8, 16, 33, 100, 257] {
            let mut agenda = Agenda::with_capacity(capacity);
            let allocated = agenda.items.capacity();
            let mut sorted: Vec<(i64, u64)> = Vec::new();
            let mut sequence = 0u64;
            for step in 0..6_000 {
                // Mostly filling for a while, then mostly emptying.
                let filling = (step / 500) % 2 == 0;
                let choice = random.below(10);
                if choice < if filling { 6 } else { 2 } {
                    // Few distinct times, so that ties are common.
                    let item = (random.below(40) as i64 - 20, sequence);
                    sequence += 1;
                    let pushed = agenda.push(item);
                    if sorted.len() < capacity {
                        assert_eq!(pushed, Ok(()));
                        let at = sorted.partition_point(|other| *other < item);
                        sorted.insert(at, item);
                    } else {
                        assert_eq!(pushed, Err(item));
                    }
                } else if choice < 8 {
                    let expected = (!sorted.is_empty()).then(|| sorted.remove(0));
                    assert_eq!(agenda.pop_first(), expected);
                } else {
                    assert_eq!(agenda.pop_last(), sorted.pop());
                }
                assert_eq!(agenda.len(), sorted.len());
                assert_eq!(agenda.first(), sorted.first());
                assert_eq!(agenda.last(), sorted.last());
                assert_well_formed(&agenda);
            }
            assert_eq!(agenda.capacity(), capacity);
            // It never asked for more memory.
            assert_eq!(agenda.items.capacity(), allocated);

            // And emptied from either end alone.
            while let Some(item) = agenda.pop_last() {
                assert_eq!(Some(item), sorted.pop());
                assert_well_formed(&agenda);
            }
            assert_eq!(agenda.pop_first(), None);
            assert_eq!((agenda.first(), agenda.last()), (None, None));
        }
    }

    #[test]
    fn items_come_out_in_order_whatever_order_they_went_in() {
        let mut random = Random(0x0000_5EED_0F0A_6E17);
        for round in 0..200 {
            let count = 1 + random.below(300) as usize;
            let mut items: Vec<(i64, u64)> = (0..count as u64)
                .map(|sequence| (random.below(50) as i64, sequence))
                .collect();
            let mut agenda = Agenda::with_capacity(count);
            for &item in &items {
                agenda.push(item).unwrap();
            }
            items.sort_unstable();
            let out: Vec<(i64, u64)> = if round % 2 == 0 {
                core::iter::from_fn(|| agenda.pop_first()).collect()
            } else {
                let mut backwards: Vec<_> = core::iter::from_fn(|| agenda.pop_last()).collect();
                backwards.reverse();
                backwards
            };
            assert_eq!(out, items);
        }
    }

    #[test]
    fn an_agenda_of_no_capacity_holds_nothing() {
        let mut agenda = Agenda::with_capacity(0);
        assert_eq!(agenda.push((1, 1)), Err((1, 1)));
        assert_eq!(agenda.pop_first(), None);
        assert_eq!(agenda.pop_last(), None);
        assert_eq!(agenda.pop_any(), None);
        assert_eq!(agenda.len(), 0);
    }
}
