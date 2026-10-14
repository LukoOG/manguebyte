//! Proves that metering never touches the heap.
//!
//! Meters run on the audio thread, where an allocation is a dropout waiting
//! to happen. As in `manguebyte-dsp`, this test replaces the global
//! allocator with one that counts, and fails if any meter allocates,
//! reallocates or frees while it measures, is reset, or is read.
//!
//! When you add a meter, add it here.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use manguebyte_analysis::{
    LagInterpolation, LoudnessMeter, OctaveFraction, PeakMeter, PitchDetector, PitchMethod,
    PitchSettings, PpmMeter, RmsMeter, SpectrumAnalyzer, SpectrumSettings, StereoMeter,
    TruePeakMeter, VuMeter, Weighting,
};
use manguebyte_core::AudioBuffer;
use manguebyte_dsp::window::Window;
use manguebyte_dsp::{Gain, ProcessSpec, Processor};

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
        "{what} used the heap {calls} time(s) while measuring"
    );
}

const SPEC: ProcessSpec = ProcessSpec {
    sample_rate: 48_000.0,
    max_block_size: 256,
    num_channels: 2,
};

/// A stereo block of something that is neither silent nor steady.
fn programme() -> AudioBuffer<f32> {
    let mut buffer = AudioBuffer::<f32>::new(2, 256);
    for (index, channel) in buffer.iter_channels_mut().enumerate() {
        for (frame, sample) in channel.iter_mut().enumerate() {
            let phase = frame as f32 * (0.05 + 0.02 * index as f32);
            *sample = 0.4 * phase.sin() + 0.1 * (7.3 * phase).sin();
        }
    }
    buffer
}

/// Runs `measure` over four seconds of blocks of several sizes with the
/// allocator watched: long enough for every window of every meter to fill.
fn four_seconds(what: &str, mut measure: impl FnMut(&mut AudioBuffer<f32>)) {
    let mut buffer = programme();
    assert_no_heap_use(what, || {
        let mut frames_done = 0;
        let mut turn = 0;
        while frames_done < 4 * 48_000 {
            let frames = [256, 1, 64, 255, 256][turn % 5];
            turn += 1;
            buffer.set_num_frames(frames);
            measure(&mut buffer);
            frames_done += frames;
        }
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
fn the_loudness_meter_does_not_use_the_heap() {
    for mut meter in [
        LoudnessMeter::stereo(),
        LoudnessMeter::surround_5_1(),
        LoudnessMeter::mono(),
    ] {
        meter.prepare(&SPEC);
        let readings = meter.readings().clone();
        let mut calls = 0;
        four_seconds("LoudnessMeter", |buffer| {
            meter.measure(buffer);
            calls += 1;
            // What a user interface does while audio runs: read, pause,
            // continue, ask for a reset.
            std::hint::black_box(readings.snapshot());
            std::hint::black_box(readings.measured_seconds());
            match calls % 500 {
                100 => readings.set_paused(true),
                150 => readings.set_paused(false),
                499 => readings.request_reset(),
                _ => {}
            }
        });
        // It did measure: gating blocks and short-term values were taken.
        assert!(readings.short_term() > -40.0);

        // The resets run on the audio thread as well.
        let mut buffer = programme();
        assert_no_heap_use("LoudnessMeter resets", || {
            meter.reset_integration();
            meter.measure(&buffer);
            meter.reset();
            Processor::<f32>::process(&mut meter, &mut buffer);
            Processor::<f32>::reset(&mut meter);
        });
    }
}

#[test]
fn the_integrated_loudness_and_the_range_do_not_use_the_heap_however_long_they_run() {
    // Five minutes, with the level wandering over 40 LU so that the
    // histograms fill up. Memory was all taken by the constructor.
    let mut meter = LoudnessMeter::stereo();
    meter.prepare(&SPEC);
    let mut buffer = AudioBuffer::<f32>::new(2, 4_800);
    assert_no_heap_use("five minutes of LoudnessMeter", || {
        for block in 0..3_000u16 {
            // A square wave is cheap to make and its level is its height.
            let level = 10f32.powf(-f32::from(block % 400) / 200.0);
            for channel in buffer.iter_channels_mut() {
                for (frame, sample) in channel.iter_mut().enumerate() {
                    *sample = if (frame / 24) % 2 == 0 { level } else { -level };
                }
            }
            meter.measure(&buffer);
        }
    });
    let readings = meter.readings();
    assert_eq!(readings.measured_seconds(), 300.0);
    assert!(readings.integrated().is_finite());
    assert!(readings.range() > 10.0);
}

#[test]
fn the_true_peak_meter_does_not_use_the_heap() {
    for factor in [2, 4, 8, 16] {
        let mut meter = TruePeakMeter::new(2).with_oversampling(factor);
        meter.prepare(&SPEC);
        let readings = meter.readings().clone();
        let mut buffer = programme();
        assert_no_heap_use("TruePeakMeter", || {
            for frames in [256, 1, 64, 255, 256, 256] {
                buffer.set_num_frames(frames);
                meter.measure(&buffer);
                std::hint::black_box(readings.max_overall_db());
                std::hint::black_box(readings.held(0));
            }
            readings.request_reset();
            meter.measure(&buffer);
            meter.reset();
            Processor::<f32>::process(&mut meter, &mut buffer);
            // The end of a stream is handled on the audio thread as well,
            // with or without a reset waiting to be carried out.
            meter.flush();
            readings.request_reset();
            meter.flush();
            meter.measure(&buffer);
        });
        assert!(readings.max(0) > 0.0);
    }
}

#[test]
fn bad_samples_are_screened_without_using_the_heap() {
    // Screening is part of every meter's inner loop. Samples that are not
    // numbers, infinite ones and ones too large to square must cost no more
    // than good ones do.
    let mut garbage = AudioBuffer::<f32>::new(2, 256);
    for (index, channel) in garbage.iter_channels_mut().enumerate() {
        for (frame, sample) in channel.iter_mut().enumerate() {
            *sample = [
                f32::NAN,
                f32::INFINITY,
                0.25,
                f32::NEG_INFINITY,
                f32::MAX,
                f32::MIN,
                -0.5,
            ][(frame + index) % 7];
        }
    }
    let mut wide = AudioBuffer::<f64>::new(2, 256);
    for channel in wide.iter_channels_mut() {
        for (frame, sample) in channel.iter_mut().enumerate() {
            *sample = [f64::MAX, 1e200, f64::NAN, -1e155, 0.1, f64::INFINITY][frame % 6];
        }
    }
    let mut chain = (
        LoudnessMeter::stereo(),
        TruePeakMeter::new(2),
        PeakMeter::new(2),
        RmsMeter::new(2),
        VuMeter::new(2),
        PpmMeter::new(2),
        StereoMeter::new(),
    );
    Processor::<f32>::prepare(&mut chain, &SPEC);
    assert_no_heap_use("meters given bad samples", || {
        for _ in 0..200 {
            Processor::<f32>::process(&mut chain, &mut garbage);
            Processor::<f64>::process(&mut chain, &mut wide);
        }
        std::hint::black_box((
            chain.0.readings().snapshot(),
            chain.1.readings().max_overall_db(),
            chain.2.readings().held_overall_db(),
            chain.3.readings().rms_db(0),
            chain.4.readings().vu(1),
            chain.5.readings().level_db(0),
            chain.6.readings().correlation(),
            chain.6.readings().balance(),
        ));
    });
    // Every reading is a number, and none is beyond what samples limited to
    // 180 dB over full scale allow. The waveform between such samples can
    // be higher than they are, which is the true-peak meter's business, and
    // the peak programme meter multiplies by its line-up factor of 1.02.
    assert!(chain.0.readings().momentary().is_finite());
    let true_peak = chain.1.readings().max_overall();
    assert!((1e9..1e10).contains(&true_peak), "{true_peak}");
    assert_eq!(chain.2.readings().max_overall(), 1e9);
    assert!(chain.3.readings().rms(0) <= 1e9);
    assert!(chain.4.readings().deflection(0).is_finite());
    assert!(chain.5.readings().level(0) < 1.1e9);
    assert!((-1.0..=1.0).contains(&chain.6.readings().correlation()));
    assert!((-1.0..=1.0).contains(&chain.6.readings().balance()));
}

#[test]
fn the_level_meters_do_not_use_the_heap() {
    let mut peak = PeakMeter::new(2);
    peak.prepare(&SPEC);
    let readings = peak.readings().clone();
    four_seconds("PeakMeter", |buffer| {
        peak.measure(buffer);
        std::hint::black_box((
            readings.level_db(0),
            readings.held(1),
            readings.max_overall(),
        ));
    });
    assert!(readings.max(0) > 0.0);

    let mut rms = RmsMeter::new(2);
    rms.prepare(&SPEC);
    let readings = rms.readings().clone();
    four_seconds("RmsMeter", |buffer| {
        rms.measure(buffer);
        std::hint::black_box(readings.rms_db(1));
    });
    assert!(readings.rms(0) > 0.0);

    let mut vu = VuMeter::new(2);
    vu.prepare(&SPEC);
    let readings = vu.readings().clone();
    four_seconds("VuMeter", |buffer| {
        vu.measure(buffer);
        std::hint::black_box(readings.vu(0));
    });
    assert!(readings.deflection(0) > 0.0);

    let mut ppm = PpmMeter::new(2);
    ppm.prepare(&SPEC);
    let readings = ppm.readings().clone();
    four_seconds("PpmMeter", |buffer| {
        ppm.measure(buffer);
        std::hint::black_box(readings.level_db(0));
    });
    assert!(readings.level(0) > 0.0);

    let mut stereo = StereoMeter::new();
    stereo.prepare(&SPEC);
    let readings = stereo.readings().clone();
    four_seconds("StereoMeter", |buffer| {
        stereo.measure(buffer);
        std::hint::black_box((
            readings.correlation(),
            readings.balance(),
            readings.level_difference_db(),
        ));
    });
    assert!(readings.correlation() != 0.0);

    // Requests and resets, for all of them.
    let mut buffer = programme();
    assert_no_heap_use("resets of the level meters", || {
        peak.readings().request_reset();
        rms.readings().request_reset();
        vu.readings().request_reset();
        ppm.readings().request_reset();
        stereo.readings().request_reset();
        peak.measure(&buffer);
        rms.measure(&buffer);
        vu.measure(&buffer);
        ppm.measure(&buffer);
        stereo.measure(&buffer);
        peak.reset();
        rms.reset();
        vu.reset();
        ppm.reset();
        stereo.reset();
        Processor::<f32>::process(&mut peak, &mut buffer);
        Processor::<f32>::process(&mut rms, &mut buffer);
        Processor::<f32>::process(&mut vu, &mut buffer);
        Processor::<f32>::process(&mut ppm, &mut buffer);
        Processor::<f32>::process(&mut stereo, &mut buffer);
    });
}

#[test]
fn the_spectrum_analyzer_does_not_use_the_heap() {
    for (size, window, fraction) in [
        (1_024, Window::Hann, OctaveFraction::TwentyFourth),
        (1_000, Window::Kaiser(9.0), OctaveFraction::Third),
        (4_097, Window::FlatTop, OctaveFraction::Whole),
    ] {
        let settings = SpectrumSettings::new(size)
            .with_hop(size / 3)
            .with_window(window)
            .with_weighting(Weighting::A)
            .with_bands(fraction)
            .with_ballistics(0.01, 0.5)
            .with_peak_hold(0.5, 20.0);
        let (mut analyzer, mut reader) = SpectrumAnalyzer::new(settings);
        analyzer.prepare(&SPEC);
        let mut calls = 0;
        four_seconds("SpectrumAnalyzer", |buffer| {
            analyzer.measure(buffer);
            calls += 1;
            // What a user interface does while audio runs, here on the
            // same thread: take the newest frame, ask for resets.
            let frame = reader.latest();
            std::hint::black_box((frame.magnitudes_db()[3], frame.bands_db().len()));
            match calls % 400 {
                100 => reader.request_peak_reset(),
                399 => reader.request_reset(),
                _ => {}
            }
        });
        assert!(analyzer.frame_count() > 0);

        let mut buffer = programme();
        assert_no_heap_use("SpectrumAnalyzer resets", || {
            analyzer.reset();
            analyzer.reset_peaks();
            analyzer.measure_samples(buffer.channel(0));
            Processor::<f32>::process(&mut analyzer, &mut buffer);
            Processor::<f32>::reset(&mut analyzer);
        });
    }
}

#[test]
fn the_pitch_detector_does_not_use_the_heap() {
    for method in [PitchMethod::yin(), PitchMethod::mpm()] {
        for interpolation in [LagInterpolation::BandLimited, LagInterpolation::Parabolic] {
            let settings = PitchSettings::new(method)
                .with_range(55.0, 2_000.0)
                .with_hop(509)
                .with_interpolation(interpolation);
            let mut detector = PitchDetector::new(settings);
            detector.prepare(&SPEC);
            let readings = detector.readings().clone();
            let mut calls = 0;
            four_seconds("PitchDetector", |buffer| {
                detector.measure(buffer);
                calls += 1;
                std::hint::black_box((readings.pitch(), readings.pitch().note()));
                if calls % 700 == 699 {
                    readings.request_reset();
                }
            });
            assert!(detector.analysis_count() > 0);
            assert!(!detector.latest().is_none());

            let mut buffer = programme();
            assert_no_heap_use("PitchDetector resets", || {
                detector.reset();
                detector.measure_samples(buffer.channel(0));
                Processor::<f32>::process(&mut detector, &mut buffer);
                Processor::<f32>::reset(&mut detector);
            });
        }
    }
}

#[test]
fn a_chain_with_every_meter_does_not_use_the_heap() {
    let (spectrum, _reader) =
        SpectrumAnalyzer::new(SpectrumSettings::new(2_048).with_bands(OctaveFraction::Third));
    // A chain is a tuple of at most eight; the last member is a chain too.
    let mut chain = (
        Gain::<f32>::from_db(-3.0),
        LoudnessMeter::stereo(),
        TruePeakMeter::new(2),
        PeakMeter::new(2),
        RmsMeter::new(2),
        VuMeter::new(2),
        PpmMeter::new(2),
        (
            StereoMeter::new(),
            spectrum,
            PitchDetector::new(PitchSettings::default()),
        ),
    );
    chain.prepare(&SPEC);
    four_seconds("a chain of meters", |buffer| chain.process(buffer));
    assert!(chain.1.readings().integrated().is_finite());
    assert!(chain.2.readings().max_overall() > 0.0);
    assert!(chain.7.1.frame_count() > 0);
    assert!(chain.7.2.analysis_count() > 0);
}

#[test]
fn meters_also_take_double_precision_blocks_without_using_the_heap() {
    let mut loudness = LoudnessMeter::stereo();
    let mut true_peak = TruePeakMeter::new(2);
    loudness.prepare(&SPEC);
    true_peak.prepare(&SPEC);
    let mut buffer = AudioBuffer::<f64>::new(2, 256);
    for channel in buffer.iter_channels_mut() {
        for (frame, sample) in channel.iter_mut().enumerate() {
            *sample = 0.3 * (frame as f64 * 0.11).sin();
        }
    }
    assert_no_heap_use("meters on f64 blocks", || {
        for _ in 0..800 {
            loudness.measure(&buffer);
            true_peak.measure(&buffer);
        }
    });
    assert!(loudness.readings().integrated().is_finite());
}
