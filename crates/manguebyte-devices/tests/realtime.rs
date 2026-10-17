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

#[cfg(feature = "midi")]
#[test]
fn mpe_from_a_port_becomes_per_note_events_without_the_heap() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use manguebyte_dsp::event::{EventBuffer, EventKind, Expression};
    use manguebyte_dsp::note_input::{Mpe, NoteInput};

    let mock = mock();
    let mut manager = DeviceManager::headless(&mock);
    manager.set_max_block_size(512);
    // What the callback saw: notes started with an identifier on the
    // master channel, and the tuning of the last expression, in cents.
    let notes = Arc::new(AtomicU64::new(0));
    let cents = Arc::new(AtomicU64::new(0));
    let (seen_notes, seen_cents) = (Arc::clone(&notes), Arc::clone(&cents));
    let mut input = NoteInput::new(Mpe::LOWER_ZONE);
    let mut events = EventBuffer::with_capacity(256);
    manager
        .start(&setup(), move |io: &mut DeviceIo<'_>| {
            events.clear();
            io.read_midi_events(&mut input, &mut events);
            for event in &events {
                match event.kind {
                    EventKind::NoteOn { note, .. } if note.id.is_some() && note.channel == 0 => {
                        seen_notes.fetch_add(1, Ordering::Relaxed);
                    }
                    EventKind::NoteExpression {
                        expression: Expression::Tuning,
                        value,
                        ..
                    } => seen_cents.store((value * 100.0).round() as u64, Ordering::Relaxed),
                    _ => {}
                }
            }
            io.output.clear();
        })
        .unwrap();
    let mut out = vec![0.0f32; 2 * 1_500];
    // Member channel 3 bends up all the way, plays, and comes half way
    // back; then member channel 4 plays.
    mock.send_midi(
        "keys",
        &[
            0xE2, 0x7F, 0x7F, 0x92, 60, 100, 0xE2, 0x00, 0x60, 0x93, 64, 90,
        ],
    );
    assert_no_heap_use("MPE from a MIDI port", || {
        for frames in SIZES {
            assert!(mock.render(&mut out[..2 * frames]));
        }
    });
    assert_eq!(notes.load(Ordering::Relaxed), 2);
    // 0x60 << 7 is a bend of 4096 above the centre: half of 48 semitones,
    // widened as MIDI 2.0 widens it.
    assert_eq!(cents.load(Ordering::Relaxed), 2_400);
}

#[test]
fn a_source_plays_in_the_device_callback_without_the_heap() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use manguebyte_core::AudioBuffer;
    use manguebyte_core::source::{AudioSource, BufferSource};
    use manguebyte_devices::{AudioCallback, StreamFormat};
    use manguebyte_dsp::sources::{ChannelRemapSource, MixerSource, ResamplingSource};

    /// Pulls a mixer into the device's buffer, prepared for the device's
    /// format, and says how loud the last block was.
    struct Player {
        mixer: MixerSource,
        peak: Arc<AtomicU32>,
    }

    impl AudioCallback for Player {
        fn prepare(&mut self, format: &StreamFormat) {
            self.mixer
                .prepare(format.sample_rate, format.max_block_size);
        }
        fn process(&mut self, io: &mut DeviceIo<'_>) {
            self.mixer.pull(io.output);
            self.peak
                .store(io.output.peak().to_bits(), Ordering::Relaxed);
        }
    }

    let mock = mock();
    let mut manager = DeviceManager::headless(&mock);
    manager.set_max_block_size(512);
    // A mono loop recorded at 44.1 kHz, on both channels of the 48 kHz
    // device.
    let mut recording = AudioBuffer::<f32>::new(1, 4_410);
    recording.channel_mut(0).fill(0.5);
    let mono = ResamplingSource::new(BufferSource::new(recording).looping(true), 44_100.0, 1);
    let mut mixer = MixerSource::new(2);
    mixer.add(ChannelRemapSource::new(mono, 1, &[Some(0), Some(0)]), 1.0);
    let peak = Arc::new(AtomicU32::new(0));
    let player = Player {
        mixer,
        peak: Arc::clone(&peak),
    };
    manager.start(&setup(), player).unwrap();
    let mut out = vec![0.0f32; 2 * 1_500];
    assert_no_heap_use("a source in a device callback", || {
        for frames in SIZES {
            assert!(mock.render(&mut out[..2 * frames]));
        }
    });
    assert!((f32::from_bits(peak.load(Ordering::Relaxed)) - 0.5).abs() < 1e-3);
}
