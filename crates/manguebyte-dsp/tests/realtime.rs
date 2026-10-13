//! Proves that processing never touches the heap.
//!
//! The real-time contract says `process` must not allocate or free memory.
//! A promise in a doc comment is easy to break by accident, so this test
//! replaces the global allocator with one that counts, and fails if any
//! processor allocates, reallocates or frees while it runs.
//!
//! When you add a processor, add it here.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use manguebyte_core::AudioBuffer;
use manguebyte_dsp::fft::{Complex, Fft, RealFft};
use manguebyte_dsp::{
    Adsr, AdsrParams, Biquad, BiquadCoefficients, Compressor, Delay, Filter, FilterShape, Gain,
    NoiseGate, Oscillator, ProcessSpec, Processor, Reverb, StateVariableFilter, SvfMode, Waveform,
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

const SPEC: ProcessSpec = ProcessSpec {
    sample_rate: 48_000.0,
    max_block_size: 256,
    num_channels: 2,
};

/// Prepares a processor, then runs it over blocks of several sizes with the
/// allocator watched.
fn check<P: Processor<f32>>(what: &str, mut processor: P) {
    processor.prepare(&SPEC);
    let mut buffer = AudioBuffer::<f32>::new(SPEC.num_channels, SPEC.max_block_size);
    assert_no_heap_use(what, || {
        for frames in [256, 1, 64, 255, 256] {
            buffer.set_num_frames(frames);
            for channel in buffer.iter_channels_mut() {
                channel.fill(0.25);
            }
            processor.process(&mut buffer);
        }
        processor.reset();
        buffer.set_num_frames(256);
        processor.process(&mut buffer);
    });
}

#[test]
fn the_guard_itself_catches_an_allocation() {
    let caught = std::panic::catch_unwind(|| {
        assert_no_heap_use("a Vec", || drop(std::hint::black_box(vec![0u8; 64])));
    });
    WATCHING.with(|watching| watching.set(false));
    assert!(caught.is_err(), "the allocation guard is not working");
}

#[test]
fn processors_do_not_use_the_heap() {
    check("Gain", Gain::<f32>::from_db(-6.0));
    check("Oscillator", Oscillator::new(Waveform::Saw, 220.0));
    check(
        "Filter",
        Filter::<f32>::new(FilterShape::Lowpass, 1_000.0, 0.707),
    );
    check(
        "Biquad",
        Biquad::<f32>::new(BiquadCoefficients::design(
            FilterShape::Peak { gain_db: 6.0 },
            48_000.0,
            2_000.0,
            1.0,
        )),
    );
    check("Adsr", {
        let mut envelope = Adsr::new(AdsrParams::default());
        envelope.note_on();
        envelope
    });
    check(
        "Delay",
        Delay::<f32>::new(0.5)
            .with_time(0.125)
            .with_feedback(0.4)
            .with_mix(0.5),
    );
}

#[test]
fn newer_processors_do_not_use_the_heap() {
    check(
        "StateVariableFilter",
        StateVariableFilter::<f32>::new(SvfMode::Bandpass, 1_000.0, 2.0),
    );
    check(
        "Compressor",
        Compressor::<f32>::new()
            .with_threshold_db(-30.0)
            .with_knee_db(6.0),
    );
    check(
        "NoiseGate",
        NoiseGate::<f32>::new().with_threshold_db(-20.0),
    );
    check("Reverb", {
        let mut reverb = Reverb::<f32>::new();
        // Settings may be changed from the audio thread too.
        reverb.set_size(0.8);
        reverb.set_decay_seconds(4.0);
        reverb
    });
}

#[test]
fn a_chain_does_not_use_the_heap_and_can_change_settings_while_running() {
    let mut chain = (
        Oscillator::new(Waveform::Square, 110.0),
        Filter::<f32>::new(FilterShape::Lowpass, 800.0, 1.2),
        Gain::<f32>::new(0.5),
    );
    chain.prepare(&SPEC);
    let mut buffer = AudioBuffer::<f32>::new(2, 256);
    assert_no_heap_use("a tuple chain", || {
        for step in 0..8 {
            // Parameter changes are part of what the audio thread does.
            chain.0.set_frequency(110.0 + 20.0 * f64::from(step));
            chain.1.set_frequency(800.0 + 300.0 * f64::from(step));
            chain.1.set_shape(FilterShape::Highpass);
            chain.2.set_gain_db(-f64::from(step));
            chain.process(&mut buffer);
        }
    });
}

#[test]
fn transforms_do_not_use_the_heap() {
    for len in [256, 480] {
        let mut fft = Fft::<f32>::new(len);
        let mut data = vec![Complex::new(0.5, -0.5); len];
        assert_no_heap_use("Fft", || {
            fft.forward(&mut data);
            fft.inverse(&mut data);
        });

        let mut real = RealFft::<f32>::new(len);
        let signal = vec![0.25f32; len];
        let mut spectrum = vec![Complex::ZERO; real.spectrum_len()];
        let mut restored = vec![0.0f32; len];
        assert_no_heap_use("RealFft", || {
            real.forward(&signal, &mut spectrum);
            real.inverse(&spectrum, &mut restored);
        });
    }
}
