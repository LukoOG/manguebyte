//! Proves that writing to a log never touches the heap.
//!
//! [`LogQueue`] promises that posting is safe on the audio thread. This test
//! replaces the global allocator with one that counts, and fails if posting
//! a message, formatted or not, kept or dropped, allocates, reallocates or
//! frees.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use manguebyte_core::log::{LogLevel, LogQueue, MESSAGE_BYTES, SLOTS};

thread_local! {
    static WATCHING: Cell<bool> = const { Cell::new(false) };
    static HEAP_CALLS: Cell<usize> = const { Cell::new(0) };
}

struct CountingAllocator;

impl CountingAllocator {
    fn record() {
        // `try_with` because the allocator is still called while a thread's
        // locals are being torn down.
        let _ = WATCHING.try_with(|watching| {
            if watching.get() {
                let _ = HEAP_CALLS.try_with(|calls| calls.set(calls.get() + 1));
            }
        });
    }
}

// SAFETY: every method forwards to the system allocator unchanged; the only
// addition is bumping a thread-local counter, which does not allocate.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::record();
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        Self::record();
        // SAFETY: the caller upholds `GlobalAlloc::dealloc`'s contract.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::record();
        // SAFETY: the caller upholds `GlobalAlloc::alloc_zeroed`'s contract.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::record();
        // SAFETY: the caller upholds `GlobalAlloc::realloc`'s contract.
        unsafe { System.realloc(pointer, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Runs `work` and returns how many times it called the allocator.
fn heap_calls(work: impl FnOnce()) -> usize {
    HEAP_CALLS.with(|calls| calls.set(0));
    WATCHING.with(|watching| watching.set(true));
    work();
    WATCHING.with(|watching| watching.set(false));
    HEAP_CALLS.with(Cell::get)
}

#[test]
fn posting_never_allocates() {
    let log = Box::new(LogQueue::new());
    let long = "x".repeat(MESSAGE_BYTES * 2);
    let calls = heap_calls(|| {
        for index in 0..SLOTS + 8 {
            // Kept until the slots run out, then counted as lost.
            log.post(LogLevel::Info, "plain text");
            log.post(LogLevel::Warning, &long);
            log.post_fmt(
                LogLevel::Error,
                format_args!("voice {index} at {:.3} dB, {}", -6.0206_f32, true),
            );
            log.post_fmt(LogLevel::Debug, format_args!("{long}{long}"));
        }
        // Filtered out.
        log.set_max_level(LogLevel::Error);
        log.post_fmt(LogLevel::Trace, format_args!("{:?}", [1.5_f64; 8]));
    });
    assert_eq!(calls, 0, "posting allocated");
    let drained = log.drain();
    assert_eq!(drained.records.len(), SLOTS);
    assert!(drained.lost > 0);
}

#[test]
fn pulling_audio_from_memory_never_allocates() {
    use manguebyte_core::AudioBuffer;
    use manguebyte_core::source::{AudioSource, BufferSource, PositionableSource};

    let mut buffer = AudioBuffer::<f32>::new(2, 1_000);
    buffer.channel_mut(0).fill(0.5);
    let mut memory = BufferSource::new(buffer).looping(true);
    memory.prepare(48_000.0, 512);
    let mut out = AudioBuffer::<f32>::new(3, 512);
    let calls = heap_calls(|| {
        for frames in [512, 1, 300, 511, 0, 512] {
            out.set_num_frames(frames);
            memory.pull(&mut out);
        }
        memory.set_loop(Some(100..200));
        memory.seek(150);
        memory.pull(&mut out);
        memory.set_loop(None);
        memory.seek(u64::MAX);
        memory.pull(&mut out);
    });
    assert_eq!(calls, 0, "pulling from memory used the heap");
}
