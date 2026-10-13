//! A wait-free single-producer single-consumer queue.
//!
//! Use it to pass messages between the audio thread and any other thread:
//! neither [`Producer::push`] nor [`Consumer::pop`] locks, allocates or blocks.
//!
//! ```
//! let (mut tx, mut rx) = manguebyte_core::spsc::channel::<u32>(8);
//! tx.push(42).unwrap();
//! assert_eq!(rx.pop(), Some(42));
//! ```

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Keeps the two indices on separate cache lines so the producer and the
/// consumer do not slow each other down through false sharing.
#[repr(align(64))]
struct CachePadded<T>(T);

struct Shared<T> {
    slots: Box<[UnsafeCell<MaybeUninit<T>>]>,
    mask: usize,
    /// Count of items ever popped. Written only by the consumer.
    head: CachePadded<AtomicUsize>,
    /// Count of items ever pushed. Written only by the producer.
    tail: CachePadded<AtomicUsize>,
}

// SAFETY: a slot is only ever touched by one side at a time. The producer
// writes slots in `tail..head + capacity` and the consumer reads slots in
// `head..tail`; the two ranges never overlap and the indices that bound them
// are published with Release/Acquire ordering.
unsafe impl<T: Send> Send for Shared<T> {}
// SAFETY: see above.
unsafe impl<T: Send> Sync for Shared<T> {}

impl<T> Drop for Shared<T> {
    fn drop(&mut self) {
        let mask = self.mask;
        let tail = *self.tail.0.get_mut();
        let mut head = *self.head.0.get_mut();
        while head != tail {
            // SAFETY: slots in `head..tail` were written and not yet read, and
            // `&mut self` guarantees nobody else is touching them.
            unsafe { self.slots[head & mask].get_mut().assume_init_drop() };
            head = head.wrapping_add(1);
        }
    }
}

/// The sending half of the queue.
pub struct Producer<T> {
    shared: Arc<Shared<T>>,
}

/// The receiving half of the queue.
pub struct Consumer<T> {
    shared: Arc<Shared<T>>,
}

/// Creates a queue holding at least `capacity` items.
///
/// The capacity is rounded up to the next power of two. This allocates; the
/// returned halves never do.
///
/// # Panics
///
/// Panics if `capacity` is zero.
pub fn channel<T>(capacity: usize) -> (Producer<T>, Consumer<T>) {
    assert!(capacity > 0, "capacity must be non-zero");
    let capacity = capacity.next_power_of_two();
    let slots = (0..capacity)
        .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
        .collect();
    let shared = Arc::new(Shared {
        slots,
        mask: capacity - 1,
        head: CachePadded(AtomicUsize::new(0)),
        tail: CachePadded(AtomicUsize::new(0)),
    });
    (
        Producer {
            shared: Arc::clone(&shared),
        },
        Consumer { shared },
    )
}

impl<T> Producer<T> {
    /// Appends `value`, or hands it back if the queue is full.
    pub fn push(&mut self, value: T) -> Result<(), T> {
        let shared = &*self.shared;
        let tail = shared.tail.0.load(Ordering::Relaxed);
        let head = shared.head.0.load(Ordering::Acquire);
        if tail.wrapping_sub(head) > shared.mask {
            return Err(value);
        }
        // SAFETY: the queue is not full, so this slot is outside `head..tail`
        // and the consumer will not read it until `tail` is published below.
        // `&mut self` rules out a second producer.
        unsafe { (*shared.slots[tail & shared.mask].get()).write(value) };
        shared.tail.0.store(tail.wrapping_add(1), Ordering::Release);
        Ok(())
    }

    /// Number of items the queue can hold.
    pub fn capacity(&self) -> usize {
        self.shared.mask + 1
    }

    /// Number of free slots. The consumer may free more at any moment.
    pub fn free_len(&self) -> usize {
        let tail = self.shared.tail.0.load(Ordering::Relaxed);
        let head = self.shared.head.0.load(Ordering::Acquire);
        self.capacity() - tail.wrapping_sub(head)
    }
}

impl<T> Consumer<T> {
    /// Removes and returns the oldest item, if any.
    pub fn pop(&mut self) -> Option<T> {
        let shared = &*self.shared;
        let head = shared.head.0.load(Ordering::Relaxed);
        let tail = shared.tail.0.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        // SAFETY: `head != tail`, so this slot was written by the producer and
        // published through `tail`. `&mut self` rules out a second consumer,
        // and the producer will not reuse the slot until `head` moves below.
        let value = unsafe { (*shared.slots[head & shared.mask].get()).assume_init_read() };
        shared.head.0.store(head.wrapping_add(1), Ordering::Release);
        Some(value)
    }

    /// Number of items waiting. The producer may add more at any moment.
    pub fn len(&self) -> usize {
        let head = self.shared.head.0.load(Ordering::Relaxed);
        let tail = self.shared.tail.0.load(Ordering::Acquire);
        tail.wrapping_sub(head)
    }

    /// Whether the queue is currently empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn preserves_order_and_reports_full() {
        let (mut tx, mut rx) = channel::<u32>(4);
        assert_eq!(tx.capacity(), 4);
        for i in 0..4 {
            tx.push(i).unwrap();
        }
        assert_eq!(tx.push(99), Err(99));
        assert_eq!(rx.len(), 4);
        for i in 0..4 {
            assert_eq!(rx.pop(), Some(i));
        }
        assert_eq!(rx.pop(), None);
        assert_eq!(tx.free_len(), 4);
    }

    #[test]
    fn capacity_rounds_up_to_power_of_two() {
        let (tx, _rx) = channel::<u8>(5);
        assert_eq!(tx.capacity(), 8);
    }

    #[test]
    fn drops_items_left_in_the_queue() {
        static DROPS: AtomicUsize = AtomicUsize::new(0);
        struct Counted;
        impl Drop for Counted {
            fn drop(&mut self) {
                DROPS.fetch_add(1, Ordering::Relaxed);
            }
        }

        let (mut tx, mut rx) = channel::<Counted>(4);
        for _ in 0..3 {
            assert!(tx.push(Counted).is_ok());
        }
        drop(rx.pop());
        assert_eq!(DROPS.load(Ordering::Relaxed), 1);
        drop((tx, rx));
        assert_eq!(DROPS.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn transfers_across_threads() {
        const COUNT: u64 = 100_000;
        let (mut tx, mut rx) = channel::<u64>(64);

        let producer = std::thread::spawn(move || {
            for i in 0..COUNT {
                let mut item = i;
                while let Err(back) = tx.push(item) {
                    item = back;
                    std::thread::yield_now();
                }
            }
        });

        let mut expected = 0;
        while expected < COUNT {
            match rx.pop() {
                Some(value) => {
                    assert_eq!(value, expected);
                    expected += 1;
                }
                None => std::thread::yield_now(),
            }
        }
        producer.join().unwrap();
        assert!(rx.is_empty());
    }
}
