//! Proves that computing curves never touches the heap.
//!
//! The curves of [`manguebyte_dsp::curve`] are meant for an interface, but
//! they are promised to be real-time safe as well: a processor's descriptor
//! can be made and evaluated anywhere. This test replaces the global
//! allocator with one that counts, as `tests/realtime.rs` does, and fails if
//! making a descriptor or filling a curve allocates, reallocates or frees.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use manguebyte_dsp::curve::{
    Axis, BiquadResponse, ChorusSweep, CompressorTransfer, Curve, DelayTaps, ExpanderTransfer,
    FirResponse, FrequencyResponse, GateTransfer, LfoCycle, LimiterTransfer, Magnitude, Phase,
    PhaserSweep, Point, ReverbDecay, unwrap_phase,
};
use manguebyte_dsp::iir::{Band, Design};
use manguebyte_dsp::{
    Adsr, AdsrParams, BiquadCascade, Chorus, Compressor, Crossover, CrossoverSlope, Delay,
    Expander, Filter, FilterShape, FirFilter, IirFilter, LadderFilter, LadderMode, Lfo, LfoShape,
    Limiter, NoiseGate, OnePole, OnePoleMode, Phaser, ProcessSpec, Processor, Reverb, ShaperCurve,
    StateVariableFilter, SvfMode, Waveshaper,
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
    assert_eq!(calls, 0, "{what} used the heap {calls} time(s)");
}

const SPEC: ProcessSpec = ProcessSpec {
    sample_rate: 48_000.0,
    max_block_size: 256,
    num_channels: 2,
};

/// Fills a curve along the axis it asks for, and checks that every value is
/// a number.
fn draw(curve: &dyn Curve, xs: &mut [f64], ys: &mut [f64]) {
    let spec = curve.spec();
    spec.x.fill(xs);
    curve.fill(xs, ys);
    assert!(ys.iter().all(|y| y.is_finite()));
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
fn frequency_responses_do_not_use_the_heap() {
    let mut filter = Filter::<f32>::new(FilterShape::Peak { gain_db: 6.0 }, 1_000.0, 2.0);
    let design = Design::elliptic(Band::Lowpass { cutoff_hz: 2_000.0 }, 8, 0.5, 80.0);
    let mut iir = IirFilter::<f32>::new(design);
    let cascade = BiquadCascade::<f32>::new(&design.sections(48_000.0));
    let mut svf = StateVariableFilter::<f32>::new(SvfMode::Notch, 3_000.0, 4.0);
    let mut one_pole = OnePole::<f32>::new(OnePoleMode::Allpass, 500.0);
    let mut ladder = LadderFilter::<f32>::new(LadderMode::Bp24, 800.0).with_resonance(0.8);
    let mut crossover = Crossover::<f32>::new(CrossoverSlope::Lr8, &[100.0, 1_000.0, 8_000.0]);
    let fir = FirFilter::<f32>::new(&manguebyte_dsp::fir::lowpass(301, 0.2, 80.0)).unwrap();
    filter.prepare(&SPEC);
    iir.prepare(&SPEC);
    svf.prepare(&SPEC);
    one_pole.prepare(&SPEC);
    ladder.prepare(&SPEC);
    crossover.prepare(&SPEC);
    let taps = [0.25, 0.5, 0.25];
    let (mut xs, mut ys) = ([0.0; 512], [0.0; 512]);

    assert_no_heap_use("frequency responses", || {
        let eq = [
            filter.response(),
            BiquadResponse::design(FilterShape::LowShelf { gain_db: -3.0 }, 120.0, 0.7, 48e3),
        ];
        let low = crossover.band_response(0).unwrap();
        let chain = (svf.response(), one_pole.response(), ladder.response());
        let responses: [&dyn FrequencyResponse; 10] = [
            &eq,
            &iir.response(),
            &design.response(96_000.0),
            &cascade.response(48_000.0),
            &chain,
            &crossover.response(),
            &low,
            &fir.response(48_000.0),
            &FirResponse::new(&taps, 48_000.0),
            &eq.as_slice(),
        ];
        for response in responses {
            draw(&Magnitude(response), &mut xs, &mut ys);
            draw(&Phase(response), &mut xs, &mut ys);
            unwrap_phase(&mut ys);
        }
    });
}

#[test]
fn transfer_curves_and_shapes_do_not_use_the_heap() {
    let compressor = Compressor::<f32>::new().with_knee_db(6.0);
    let expander = Expander::<f32>::new();
    let gate = NoiseGate::<f32>::new().with_floor_db(-30.0);
    let limiter = Limiter::<f32>::new();
    let shaper = Waveshaper::<f32>::new(ShaperCurve::Fold).with_drive_db(12.0);
    let adsr = Adsr::new(AdsrParams::default());
    let lfo = Lfo::new(LfoShape::SampleAndHold, 2.0);
    let mut chorus = Chorus::<f32>::flanger();
    let mut phaser = Phaser::<f32>::new().with_spread(0.5);
    let mut delay = Delay::<f32>::new(1.0).with_time(0.3).with_feedback(0.6);
    let mut reverb = Reverb::<f32>::new().with_damping(0.6);
    chorus.prepare(&SPEC);
    phaser.prepare(&SPEC);
    delay.prepare(&SPEC);
    reverb.prepare(&SPEC);
    let (mut xs, mut ys) = ([0.0; 512], [0.0; 512]);
    let mut taps = [Point::default(); 64];

    assert_no_heap_use("transfer curves and shapes", || {
        let compressor = compressor.transfer();
        let expander = expander.transfer();
        let gate = gate.transfer();
        let limiter = limiter.transfer();
        let envelope = adsr.shape(0.5);
        let cycle = lfo.cycle();
        let sweep = chorus.sweep(0, 1).unwrap();
        let stages = phaser.sweep(1);
        let decay = reverb.decay();
        let energy = decay.energy_at(4_000.0);
        let curves: [&dyn Curve; 17] = [
            &compressor,
            &CompressorTransfer::new(-24.0, 3.0, 6.0, 4.0),
            &expander,
            &ExpanderTransfer::new(-50.0, 2.0, 0.0, 40.0),
            &gate,
            &GateTransfer::new(-40.0, f64::NEG_INFINITY),
            &limiter,
            &LimiterTransfer::new(-1.0),
            &shaper,
            &ShaperCurve::Tanh,
            &envelope,
            &cycle,
            &LfoCycle::new(LfoShape::Triangle),
            &sweep,
            &stages,
            &decay,
            &energy,
        ];
        for curve in curves {
            draw(curve, &mut xs, &mut ys);
        }
        let more: [&dyn Curve; 3] = [
            &ChorusSweep::new(15.0, 3.0, 0.5, 48_000.0),
            &PhaserSweep::new(800.0, 2.0, 0.3, 48_000.0),
            &ReverbDecay::new(3.0, 0.5, 0.7, 48_000.0),
        ];
        for curve in more {
            draw(curve, &mut xs, &mut ys);
        }
        let count = delay.taps().fill(&mut taps);
        assert!(count > 10);
        let count = DelayTaps::new(0.2, 0.5, 0.5, 48_000.0).fill(&mut taps);
        assert!(count > 10);
        let _ = delay.taps().spec();
        Axis::frequency(44_100.0).fill(&mut xs);
    });
}
