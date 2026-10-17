//! Proves that the path from a device to the callback never touches the
//! heap: the block adapters, the duplex bridge with its drift correction,
//! the device manager's engine and its MIDI.
//!
//! The headless backend drives that path exactly as a sound card's thread
//! would, through the same code, while a counting allocator watches.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use manguebyte_devices::{
    DeviceChoice, DeviceIo, DeviceManager, DeviceSetup, Direction, MockBackend,
};

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

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::record();
        // SAFETY: the caller upholds `GlobalAlloc::realloc`'s contract.
        unsafe { System.realloc(pointer, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Runs `work` and panics if it used the heap in any way.
fn assert_no_heap_use(what: &str, work: impl FnOnce()) {
    HEAP_CALLS.with(|calls| calls.set(0));
    WATCHING.with(|watching| watching.set(true));
    work();
    WATCHING.with(|watching| watching.set(false));
    let calls = HEAP_CALLS.with(Cell::get);
    assert_eq!(
        calls, 0,
        "{what} used the heap {calls} time(s) while processing"
    );
}

#[test]
fn the_guard_itself_catches_an_allocation() {
    let caught = std::panic::catch_unwind(|| {
        assert_no_heap_use("a Vec", || drop(std::hint::black_box(vec![0u8; 64])));
    });
    WATCHING.with(|watching| watching.set(false));
    assert!(caught.is_err(), "the allocation guard is not working");
}

fn mock() -> MockBackend {
    let mock = MockBackend::new();
    mock.add_device(
        Direction::Output,
        MockBackend::device("out", "Out", 2, 48_000),
    );
    mock.add_device(Direction::Input, MockBackend::device("in", "In", 2, 48_000));
    #[cfg(feature = "midi")]
    mock.add_midi_port(Direction::Input, "keys", "Keys");
    mock
}

/// A callback that does a little of everything a real one does.
fn callback(io: &mut DeviceIo<'_>) {
    #[cfg(feature = "midi")]
    io.read_midi(|offset, _| {
        std::hint::black_box(offset);
    });
    let frames = io.num_frames();
    for (index, channel) in io.output.iter_channels_mut().enumerate() {
        let source = (io.input.num_channels() > 0).then(|| io.input.channel(index % 2));
        for (frame, sample) in channel[..frames].iter_mut().enumerate() {
            *sample = source.map_or(0.1, |input| input[frame]) + io.info.frame as f32 * 1e-9;
        }
    }
}

fn setup() -> DeviceSetup {
    let setup = DeviceSetup::default();
    #[cfg(feature = "midi")]
    let setup = setup.with_midi_input(manguebyte_devices::DeviceRef::new("keys", "Keys"));
    setup
}

/// Sizes the device asks for, including some over the largest block, which
/// the stream splits.
const SIZES: [usize; 6] = [256, 1, 64, 255, 512, 1_500];

#[test]
fn output_streams_do_not_use_the_heap() {
    let mock = mock();
    let mut manager = DeviceManager::headless(&mock);
    manager.set_max_block_size(512);
    manager.start(&setup(), callback).unwrap();
    let mut out = vec![0.0f32; 2 * 1_500];
    #[cfg(feature = "midi")]
    mock.send_midi("keys", &[0x90, 60, 100, 0x80, 60, 0]);
    assert_no_heap_use("an output stream", || {
        for frames in SIZES {
            assert!(mock.render(&mut out[..2 * frames]));
        }
    });
}

#[test]
fn input_streams_do_not_use_the_heap() {
    let mock = mock();
    let mut manager = DeviceManager::headless(&mock);
    manager.set_max_block_size(512);
    let setup = setup()
        .with_output(DeviceChoice::None)
        .with_input(DeviceChoice::Default);
    manager.start(&setup, callback).unwrap();
    let captured = vec![0.25f32; 2 * 1_500];
    assert_no_heap_use("an input stream", || {
        for frames in SIZES {
            assert!(mock.capture(&captured[..2 * frames]));
        }
    });
}

#[test]
fn duplex_streams_do_not_use_the_heap() {
    for drift_correction in [true, false] {
        let mock = mock();
        let mut manager = DeviceManager::headless(&mock);
        manager.set_max_block_size(512);
        manager.set_drift_correction(drift_correction);
        manager
            .start(&setup().with_input(DeviceChoice::Default), callback)
            .unwrap();
        let captured = vec![0.25f32; 2 * 1_500];
        let mut out = vec![0.0f32; 2 * 1_500];
        #[cfg(feature = "midi")]
        mock.send_midi("keys", &[0x90, 60, 100]);
        assert_no_heap_use("a duplex stream", || {
            for _ in 0..3 {
                for frames in SIZES {
                    assert!(mock.capture(&captured[..2 * frames]));
                    assert!(mock.render(&mut out[..2 * frames]));
                }
                // The input falls behind, and catches up.
                assert!(mock.render(&mut out[..2 * 512]));
                assert!(mock.capture(&captured[..2 * 1_500]));
            }
        });
    }
}
