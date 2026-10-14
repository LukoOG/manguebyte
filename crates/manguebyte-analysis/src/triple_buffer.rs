//! A wait-free buffer that hands the latest value from one thread to
//! another.
//!
//! An atomic carries one number. A spectrum is thousands of numbers that
//! belong together, and a reader must never see half of one frame and half
//! of the next. A queue would keep them together, but a queue delivers every
//! frame, and a display that redraws sixty times a second wants only the
//! newest one.
//!
//! A triple buffer does exactly that with three copies of the value. At any
//! moment the writer owns one, the reader owns one, and the third waits in
//! the middle. The writer fills its copy and swaps it with the middle one;
//! the reader swaps its copy with the middle one when the middle one is
//! newer. Each swap is a single atomic instruction, so neither side ever
//! waits, loops or allocates, whatever the other side is doing.
//!
//! ```
//! use manguebyte_analysis::triple_buffer;
//!
//! let (mut writer, mut reader) = triple_buffer::channel([0.0f32; 4]);
//!
//! // The audio thread fills its copy in place and publishes it.
//! writer.pending_mut().fill(0.5);
//! writer.publish();
//! writer.pending_mut().fill(0.7);
//! writer.publish();
//!
//! // Any other thread takes the newest complete value, when there is one.
//! assert!(reader.update());
//! assert_eq!(reader.get(), &[0.7; 4]);
//! assert!(!reader.update());
//! ```
//!
//! # Limits
//!
//! There is one writer and one reader. Values the reader did not fetch in
//! time are overwritten, which is the point. The copy the writer gets back
//! after a swap holds an old value, not the one it last wrote, so a writer
//! must write the whole value every time and keep any running state of its
//! own elsewhere.

use core::cell::UnsafeCell;
use core::fmt;
use core::marker::PhantomData;
use core::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

/// The bits of the shared byte that say which copy is in the middle.
const INDEX: u8 = 0b011;

/// Set in the shared byte while the middle copy is newer than the reader's.
const FRESH: u8 = 0b100;

/// Keeps what the two threads write on separate cache lines, so that they do
/// not slow each other down through false sharing.
#[repr(align(64))]
struct CachePadded<T>(T);

struct Shared<T> {
    slots: [CachePadded<UnsafeCell<T>>; 3],
    /// The index of the copy that neither side owns, and [`FRESH`].
    middle: CachePadded<AtomicU8>,
}

// SAFETY: the three indices held by the writer (`back`), by the reader
// (`front`) and by `middle` are always a permutation of 0, 1 and 2: they
// start that way, and the only operation on them is an atomic swap between
// one side's index and `middle`. A slot is touched only by the side that
// holds its index, so no slot is ever accessed from two threads at once. A
// slot does move from one thread to the other, which is why `T: Send` is
// required. The swaps use `AcqRel`, so everything one side did to a slot
// before giving it up happens before anything the other side does with it.
unsafe impl<T: Send> Send for Shared<T> {}
// SAFETY: see above. `&Shared<T>` never gives two threads access to the
// same `T`, so `T: Sync` is not needed here; `Reader` adds that bound for
// the shared references it hands out itself.
unsafe impl<T: Send> Sync for Shared<T> {}

/// The writing end of a triple buffer. See the [module](self) documentation.
///
/// Nothing here locks, waits or allocates.
pub struct Writer<T> {
    shared: Arc<Shared<T>>,
    /// The slot this side owns.
    back: u8,
}

/// The reading end of a triple buffer. See the [module](self) documentation.
///
/// Nothing here locks, waits or allocates.
///
/// A reader can be moved to another thread when `T` can. It can be shared
/// between threads only when `T` can, because [`get`](Self::get) hands out
/// references to the value:
///
/// ```compile_fail
/// use core::cell::Cell;
/// use manguebyte_analysis::triple_buffer;
///
/// fn shared_between_threads<T: Sync>(_: &T) {}
/// let (_writer, reader) = triple_buffer::channel(Cell::new(0u8));
/// shared_between_threads(&reader);
/// ```
pub struct Reader<T> {
    shared: Arc<Shared<T>>,
    /// The slot this side owns.
    front: u8,
    /// Makes the reader `Sync` only when `T` is.
    _values: PhantomData<T>,
}

/// Creates a triple buffer whose three copies start as clones of `initial`.
///
/// The reader sees `initial` until the writer publishes something. This
/// allocates; the two ends never do.
pub fn channel<T: Clone>(initial: T) -> (Writer<T>, Reader<T>) {
    channel_with(|| initial.clone())
}

/// Creates a triple buffer whose three copies are made by calling `make`
/// three times, for values that cannot be cloned or should not be.
///
/// This allocates; the two ends never do.
pub fn channel_with<T>(mut make: impl FnMut() -> T) -> (Writer<T>, Reader<T>) {
    let shared = Arc::new(Shared {
        slots: [
            CachePadded(UnsafeCell::new(make())),
            CachePadded(UnsafeCell::new(make())),
            CachePadded(UnsafeCell::new(make())),
        ],
        middle: CachePadded(AtomicU8::new(1)),
    });
    (
        Writer {
            shared: Arc::clone(&shared),
            back: 0,
        },
        Reader {
            shared,
            front: 2,
            _values: PhantomData,
        },
    )
}

impl<T> Writer<T> {
    /// The copy that the next [`publish`](Self::publish) will hand over, to
    /// be filled in place. Real-time safe.
    ///
    /// What it holds before it is filled is an older value: the initial one,
    /// or one published earlier. Overwrite all of it.
    pub fn pending_mut(&mut self) -> &mut T {
        let slot = self.shared.slots[usize::from(self.back)].0.get();
        // SAFETY: the writer owns the slot `back` until it swaps it away in
        // `publish`, and nobody else can reach it (see `Shared`). The
        // reference borrows `self` mutably, so it is gone before `publish`
        // can run and no second reference to the slot can be made meanwhile.
        unsafe { &mut *slot }
    }

    /// Makes the pending copy the newest value and takes another copy to
    /// write the next one into. Real-time safe: one atomic swap.
    pub fn publish(&mut self) {
        let previous = self
            .shared
            .middle
            .0
            .swap(self.back | FRESH, Ordering::AcqRel);
        self.back = previous & INDEX;
    }

    /// Stores `value` and publishes it.
    ///
    /// The value that was in the pending copy is dropped here. If dropping a
    /// `T` frees memory, this is not for the audio thread; fill
    /// [`pending_mut`](Self::pending_mut) in place instead.
    pub fn write(&mut self, value: T) {
        *self.pending_mut() = value;
        self.publish();
    }
}

impl<T> Reader<T> {
    /// Whether the writer has published a value this reader has not fetched.
    pub fn has_update(&self) -> bool {
        self.shared.middle.0.load(Ordering::Acquire) & FRESH != 0
    }

    /// Fetches the newest published value, if there is one this reader has
    /// not fetched yet. Returns whether there was.
    ///
    /// Wait-free: one atomic load, and one atomic swap when there is news.
    pub fn update(&mut self) -> bool {
        if !self.has_update() {
            return false;
        }
        // Between the load above and this swap only the writer can have
        // changed `middle`, and it always sets `FRESH`. So the slot that
        // comes out of the swap is the newest published one.
        let previous = self.shared.middle.0.swap(self.front, Ordering::AcqRel);
        self.front = previous & INDEX;
        true
    }

    /// The value fetched by the last [`update`](Self::update): the initial
    /// one until something has been published and fetched.
    pub fn get(&self) -> &T {
        let slot = self.shared.slots[usize::from(self.front)].0.get();
        // SAFETY: the reader owns the slot `front` until it swaps it away in
        // `update`, and nobody else can reach it (see `Shared`). `update`
        // needs `&mut self`, so it cannot run while this reference is alive.
        // Nothing writes to the slot while the reader owns it, so any number
        // of shared references to it may exist; they reach other threads
        // only if the reader is `Sync`, which it is only for `T: Sync`.
        unsafe { &*slot }
    }

    /// Fetches the newest published value and returns it:
    /// [`update`](Self::update) followed by [`get`](Self::get).
    pub fn latest(&mut self) -> &T {
        self.update();
        self.get()
    }
}

impl<T> fmt::Debug for Writer<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Writer").finish_non_exhaustive()
    }
}

impl<T> fmt::Debug for Reader<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Reader")
            .field("has_update", &self.has_update())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::AtomicUsize;

    use super::*;

    /// The three indices as the two ends and the middle hold them now.
    fn indices<T>(writer: &Writer<T>, reader: &Reader<T>) -> [u8; 3] {
        let middle = writer.shared.middle.0.load(Ordering::Relaxed) & INDEX;
        [writer.back, middle, reader.front]
    }

    fn is_a_permutation(mut indices: [u8; 3]) -> bool {
        indices.sort_unstable();
        indices == [0, 1, 2]
    }

    #[test]
    fn the_reader_sees_the_initial_value_until_something_is_published() {
        let (mut writer, mut reader) = channel(7u32);
        assert!(!reader.has_update());
        assert!(!reader.update());
        assert_eq!(*reader.get(), 7);

        // Filling the pending copy is not publishing it.
        *writer.pending_mut() = 8;
        assert!(!reader.update());
        assert_eq!(*reader.get(), 7);

        writer.publish();
        assert!(reader.has_update());
        assert!(reader.update());
        assert_eq!(*reader.get(), 8);
        // Fetched once, it is no longer news, and it stays readable.
        assert!(!reader.update());
        assert_eq!(*reader.get(), 8);
        assert_eq!(*reader.latest(), 8);
    }

    #[test]
    fn the_reader_gets_the_newest_value_and_skips_the_ones_it_missed() {
        let (mut writer, mut reader) = channel(0u32);
        for value in 1..=5 {
            writer.write(value);
        }
        assert_eq!(*reader.latest(), 5);
        writer.write(6);
        writer.write(7);
        assert!(reader.update());
        assert_eq!(*reader.get(), 7);
    }

    #[test]
    fn every_copy_always_has_exactly_one_owner() {
        // Every order of publishes and updates, eight steps deep: the three
        // indices stay a permutation, the reader never goes back in time,
        // and after an update it holds the last value published.
        for pattern in 0u32..256 {
            let (mut writer, mut reader) = channel(0u32);
            assert!(is_a_permutation(indices(&writer, &reader)));
            let mut published = 0;
            let mut seen = 0;
            for step in 0..8 {
                if pattern >> step & 1 == 0 {
                    published += 1;
                    *writer.pending_mut() = published;
                    writer.publish();
                } else {
                    let news = reader.update();
                    assert_eq!(news, *reader.get() != seen, "pattern {pattern:#b}");
                    assert_eq!(*reader.get(), published, "pattern {pattern:#b}");
                    seen = *reader.get();
                }
                assert!(
                    is_a_permutation(indices(&writer, &reader)),
                    "pattern {pattern:#b}, step {step}"
                );
            }
        }
    }

    #[test]
    fn copies_made_by_a_closure_are_dropped_exactly_once() {
        static LIVE: AtomicUsize = AtomicUsize::new(0);
        struct Counted(u8);
        impl Drop for Counted {
            fn drop(&mut self) {
                LIVE.fetch_sub(1, Ordering::Relaxed);
            }
        }
        let make = || {
            LIVE.fetch_add(1, Ordering::Relaxed);
            Counted(0)
        };

        let (mut writer, mut reader) = channel_with(make);
        assert_eq!(LIVE.load(Ordering::Relaxed), 3);
        // A value written over an old one drops the old one, nothing more.
        writer.write(make());
        writer.write(make());
        assert_eq!(LIVE.load(Ordering::Relaxed), 3);
        assert_eq!(reader.latest().0, 0);

        // One end outliving the other keeps all three copies alive.
        drop(writer);
        assert_eq!(LIVE.load(Ordering::Relaxed), 3);
        assert_eq!(reader.get().0, 0);
        drop(reader);
        assert_eq!(LIVE.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn both_ends_may_be_moved_to_another_thread() {
        fn moves<T: Send>() {}
        fn shares<T: Sync>() {}
        moves::<Writer<Vec<f32>>>();
        moves::<Reader<Vec<f32>>>();
        // A value that is `Send` but not `Sync` can still cross.
        moves::<Writer<core::cell::Cell<u8>>>();
        moves::<Reader<core::cell::Cell<u8>>>();
        shares::<Writer<Vec<f32>>>();
        shares::<Reader<Vec<f32>>>();
    }

    #[test]
    fn the_ends_can_be_printed_without_printing_the_value() {
        let (writer, reader) = channel(1u8);
        assert_eq!(format!("{writer:?}"), "Writer { .. }");
        assert_eq!(format!("{reader:?}"), "Reader { has_update: false, .. }");
    }
}
