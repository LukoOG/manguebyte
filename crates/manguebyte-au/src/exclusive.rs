//! A value one thread at a time may use, which the audio thread takes
//! without allocating, locking the system or waiting.
//!
//! The standard mutex will not do on the audio thread: besides being a
//! lock, on macOS it allocates its system mutex the first time it is
//! locked, and a unit's first render is very often that first time. This
//! is the smallest thing that does the job instead: a flag that says
//! whether someone holds the value, taken with one compare-and-swap.
//!
//! - The audio thread only ever [tries](Exclusive::try_take): if another
//!   thread holds the value, it is told no at once and does without it for
//!   that call (the slice is silent, the event is dropped and counted).
//! - Other threads [take](Exclusive::take) it, yielding their time slice
//!   until it is free. The audio thread holds it for one render at most,
//!   so they wait no longer than that.
//!
//! It is not reentrant: a thread that holds the value and takes it again
//! waits for ever. Nothing the wrapper calls while holding one, the
//! plugin's code included, can reach back into the unit, and the host's
//! callbacks (listeners, render notifications) are called with nothing
//! held.

use std::cell::UnsafeCell;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, Ordering};

/// A value behind an ownership flag.
pub(crate) struct Exclusive<T> {
    held: AtomicBool,
    value: UnsafeCell<T>,
}

// SAFETY: the value is only ever reached through a `Held`, and at most one
// `Held` exists at a time: the flag is set by a successful compare-and-swap
// from `false` and cleared only by that `Held`'s drop. The acquire and
// release orderings make everything one holder wrote visible to the next.
// So sharing an `Exclusive` between threads moves the value between them,
// one at a time, which needs `T: Send` and no more.
unsafe impl<T: Send> Sync for Exclusive<T> {}
// SAFETY: as above; sending the `Exclusive` sends the value.
unsafe impl<T: Send> Send for Exclusive<T> {}

/// The value, while this thread holds it. Dropping it lets go.
pub(crate) struct Held<'a, T> {
    owner: &'a Exclusive<T>,
}

impl<T> Exclusive<T> {
    pub(crate) const fn new(value: T) -> Self {
        Self {
            held: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    /// The value, if no other thread holds it. Wait-free: one atomic
    /// operation, no allocation, no system call. For the audio thread.
    pub(crate) fn try_take(&self) -> Option<Held<'_, T>> {
        self.held
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| Held { owner: self })
    }

    /// The value, once no other thread holds it, yielding meanwhile. Not
    /// for the audio thread.
    pub(crate) fn take(&self) -> Held<'_, T> {
        loop {
            if let Some(held) = self.try_take() {
                return held;
            }
            std::thread::yield_now();
        }
    }
}

impl<T> Deref for Held<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: this `Held` is the only one there is (see the `Sync`
        // implementation), and it lives no longer than the `Exclusive`.
        unsafe { &*self.owner.value.get() }
    }
}

impl<T> DerefMut for Held<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: as above, and `&mut self` makes the borrow unique.
        unsafe { &mut *self.owner.value.get() }
    }
}

impl<T> Drop for Held<'_, T> {
    fn drop(&mut self) {
        // Also on the way out of a panic, so a value is never left held.
        self.owner.held.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn one_holder_at_a_time_and_a_panic_lets_go() {
        let cell = Exclusive::new(1);
        {
            let mut held = cell.try_take().unwrap();
            *held += 1;
            assert!(cell.try_take().is_none(), "taken twice");
        }
        assert_eq!(*cell.take(), 2);
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _held = cell.take();
            panic!("inside");
        }));
        assert!(caught.is_err());
        assert!(cell.try_take().is_some(), "left held after a panic");
    }

    #[test]
    fn threads_that_take_it_never_meet() {
        let shared = Arc::new(Exclusive::new((0u64, 0u64)));
        let rounds = if cfg!(miri) { 50 } else { 20_000 };
        let workers: Vec<_> = (0..4)
            .map(|index| {
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || {
                    for _ in 0..rounds {
                        // Half of them try, as the audio thread does, and
                        // half wait, as the others do.
                        let held = if index % 2 == 0 {
                            shared.try_take()
                        } else {
                            Some(shared.take())
                        };
                        if let Some(mut pair) = held {
                            // Both halves move together, or someone else
                            // was inside at the same time.
                            assert_eq!(pair.0, pair.1);
                            pair.0 += 1;
                            pair.1 += 1;
                        }
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let pair = shared.take();
        assert_eq!(pair.0, pair.1);
        assert!(
            pair.0 >= 2 * rounds as u64,
            "the waiting threads all got in"
        );
    }
}
