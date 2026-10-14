//! The pitch detector measured on synthesised tones from 55 Hz to 2 kHz:
//! sines, sawtooths, square waves, tones without their fundamental and
//! tones with a strong second harmonic, with both methods and both kinds
//! of interpolation; and on noise, silence and tones outside the limits.
//!
//! Run with `--nocapture` to see the measured errors.

mod common;

use common::{Noise, ToneKind as Kind, harmonic_tone as tone};
use manguebyte_analysis::{
    LagInterpolation, Note, Pitch, PitchDetector, PitchMethod, PitchSettings, Source,
};
use manguebyte_core::AudioBuffer;
use manguebyte_dsp::ProcessSpec;

/// The test tones. Every harmonic below half the sample rate is present.
const KINDS: [Kind; 6] = [
    Kind::Sine,
    Kind::Saw,
    Kind::Square,
    Kind::MissingFundamental,
    Kind::StrongSecond(2.0),
    Kind::StrongSecond(3.0),
];

fn spec(sample_rate: f64) -> ProcessSpec {
    ProcessSpec {
        sample_rate,
        max_block_size: 512,
        num_channels: 1,
    }
}

fn detector(
    method: PitchMethod,
    interpolation: LagInterpolation,
    sample_rate: f64,
) -> PitchDetector {
    let settings = PitchSettings::new(method)
        .with_range(50.0, 2_000.0)
        .with_interpolation(interpolation);
    let mut detector = PitchDetector::new(settings);
    detector.prepare(&spec(sample_rate));
    detector
}

/// The frequencies tried: every `step`-th semitone from A1 (55 Hz) to B6
/// (1975.5 Hz), 2 kHz itself, and some that lie between semitones.
fn frequencies(step: usize) -> Vec<f64> {
    let mut list: Vec<f64> = (0..=62)
        .step_by(step)
        .map(|semitone| 55.0 * 2f64.powf(semitone as f64 / 12.0))
        .collect();
    list.extend([57.3, 101.1, 333.3, 777.7, 1_234.5, 1_999.0, 2_000.0]);
    list
}

fn cents(measured: f64, expected: f64) -> f64 {
    1_200.0 * (measured / expected).log2()
}

/// The four detectors every tone is given.
const VARIANTS: [(PitchMethod, LagInterpolation); 4] = [
    (PitchMethod::yin(), LagInterpolation::BandLimited),
    (PitchMethod::mpm(), LagInterpolation::BandLimited),
    (PitchMethod::yin(), LagInterpolation::Parabolic),
    (PitchMethod::mpm(), LagInterpolation::Parabolic),
];

/// For each of `variants`, the worst error in cents and the lowest
/// confidence over [`frequencies`]`(step)`, for one kind of tone and two
/// sets of phases. Each detector analyses exactly one frame of the tone.
fn survey(
    kind: Kind,
    sample_rate: f64,
    variants: &[(PitchMethod, LagInterpolation)],
    step: usize,
) -> Vec<(f64, f64)> {
    let mut detectors: Vec<PitchDetector> = variants
        .iter()
        .map(|&(method, interpolation)| detector(method, interpolation, sample_rate))
        .collect();
    let frames = detectors[0].frame_len();
    let mut results = vec![(0.0f64, 1.0f64); variants.len()];
    for frequency in frequencies(step) {
        for phase in [0.0, 0.7] {
            let signal = tone(kind, frequency, sample_rate, frames, phase);
            for ((detector, result), variant) in
                detectors.iter_mut().zip(&mut results).zip(variants)
            {
                assert_eq!(detector.frame_len(), frames);
                detector.reset();
                detector.measure_samples(&signal);
                let pitch = detector.latest();
                let error = cents(pitch.frequency, frequency);
                assert!(
                    error.is_finite(),
                    "{variant:?} {kind:?} at {frequency} Hz: no pitch ({pitch:?})"
                );
                result.0 = result.0.max(error.abs());
                result.1 = result.1.min(pitch.confidence);
            }
        }
    }
    results
}

#[test]
fn every_test_tone_is_found_within_a_cent() {
    let mut rich_parabolic = 0.0f64;
    for kind in KINDS {
        let results = survey(kind, 48_000.0, &VARIANTS, 1);
        for ((method, interpolation), (worst, lowest)) in VARIANTS.iter().zip(results) {
            println!(
                "{method:?}, {interpolation:?}, {kind:?}: worst {worst:.3} cents, lowest confidence {lowest:.3}"
            );
            assert!(lowest > 0.9, "{method:?} {kind:?}: confidence {lowest}");
            if *interpolation == LagInterpolation::BandLimited {
                // Within a cent, which also rules out every octave error.
                assert!(worst < 1.0, "{method:?} {kind:?}: {worst} cents");
            } else {
                // What both papers do, kept as an option: as good on a
                // sine, several cents off on tones rich in harmonics near
                // 2 kHz, and still free of octave errors.
                assert!(worst < 6.0, "{method:?} {kind:?}: {worst} cents");
                if kind == Kind::Sine {
                    assert!(worst < 0.5, "{method:?} sine: {worst} cents");
                } else {
                    rich_parabolic = rich_parabolic.max(worst);
                }
            }
        }
    }
    println!("parabolic interpolation, worst on rich tones: {rich_parabolic:.3} cents");
    assert!(rich_parabolic > 2.0, "{rich_parabolic}");
}

#[test]
fn other_sample_rates_are_as_accurate() {
    // At 96 kHz the harmonics that matter lie far below half the sample
    // rate, where the interpolation is exact: 0.2 cents, as the
    // documentation's table says. Lower rates are in pitch_limits.rs.
    for (sample_rate, allowed) in [(44_100.0, 1.0), (96_000.0, 0.2)] {
        for kind in [Kind::Sine, Kind::Saw, Kind::MissingFundamental] {
            // Every other semitone, band-limited interpolation only.
            let results = survey(kind, sample_rate, &VARIANTS[..2], 2);
            for ((method, _), (worst, lowest)) in VARIANTS.iter().zip(results) {
                println!(
                    "{sample_rate} Hz, {method:?}, {kind:?}: worst {worst:.3} cents, lowest confidence {lowest:.3}"
                );
                assert!(
                    worst < allowed,
                    "{sample_rate} {method:?} {kind:?}: {worst} cents"
                );
                assert!(lowest > 0.97);
            }
        }
    }
}

#[test]
fn noise_has_a_low_confidence() {
    // Two seconds of white noise, read after every analysis.
    for method in [PitchMethod::yin(), PitchMethod::mpm()] {
        let mut detector = detector(method, LagInterpolation::BandLimited, 48_000.0);
        let mut noise = Noise(11);
        let signal: Vec<f32> = (0..96_000).map(|_| (0.5 * noise.next()) as f32).collect();
        let mut highest = 0.0f64;
        let mut analyses = 0;
        for chunk in signal.chunks(256) {
            detector.measure_samples(chunk);
            if detector.analysis_count() > 0 {
                highest = highest.max(detector.latest().confidence);
                analyses += 1;
            }
        }
        println!(
            "{method:?}: highest confidence over {analyses} analyses of white noise {highest:.3}"
        );
        assert!(analyses > 300);
        assert!(highest < 0.3, "{method:?}: {highest}");
    }
}

#[test]
fn silence_has_no_pitch() {
    for method in [PitchMethod::yin(), PitchMethod::mpm()] {
        let mut detector = detector(method, LagInterpolation::BandLimited, 48_000.0);
        let frame = detector.frame_len();
        detector.measure_samples(&tone(Kind::Saw, 220.0, 48_000.0, frame, 0.0));
        assert!((detector.latest().frequency - 220.0).abs() < 0.1);
        // Once a whole frame of silence has gone by, and the analysis that
        // follows it, there is no pitch.
        let after = frame + detector.settings().hop;
        detector.measure_samples(&vec![0.0f32; after]);
        assert_eq!(detector.latest(), Pitch::NONE);
        assert!(detector.readings().pitch().is_none());
        // Below -200 dBFS is silence as well.
        detector.measure_samples(&vec![1e-12f32; after]);
        assert_eq!(detector.latest(), Pitch::NONE);
    }
}

#[test]
fn only_periods_within_the_limits_are_reported() {
    // 3 kHz is above the limit of 2 kHz: its period of 16 samples is too
    // short, and the shortest period inside the limits that fits it is
    // twice as long.
    for method in [PitchMethod::yin(), PitchMethod::mpm()] {
        let mut detector = detector(method, LagInterpolation::BandLimited, 48_000.0);
        let frames = detector.frame_len();
        detector.measure_samples(&tone(Kind::Saw, 3_000.0, 48_000.0, frames, 0.0));
        let pitch = detector.latest();
        assert!(
            cents(pitch.frequency, 1_500.0).abs() < 1.0,
            "{method:?}: {pitch:?}"
        );
    }
    // 40 Hz is below the limit of 50 Hz. YIN finds no dip inside the
    // limits, only a slope down to the longest period, and the McLeod
    // method no key maximum: neither reports a pitch. (YIN used to report
    // the end of the slope, 50 Hz, with a low confidence; just below the
    // limit that confidence was high, so it no longer does.) The same holds
    // just below the limit, and for a sawtooth, whose slope has ripples and
    // flat stretches that YIN must not take for dips.
    for method in [PitchMethod::yin(), PitchMethod::mpm()] {
        let mut detector = detector(method, LagInterpolation::BandLimited, 48_000.0);
        let frames = detector.frame_len();
        for kind in [Kind::Sine, Kind::Saw] {
            for frequency in [30.0, 40.0, 45.0, 47.0, 48.0, 49.0, 49.5, 49.9] {
                detector.reset();
                detector.measure_samples(&tone(kind, frequency, 48_000.0, frames, 0.3));
                assert_eq!(
                    detector.latest(),
                    Pitch::NONE,
                    "{method:?} {kind:?} at {frequency} Hz"
                );
            }
            // At the limit and a hair below it, whose period is within a
            // sample of the longest, the tone is found.
            for frequency in [49.98, 50.0, 50.05, 50.5] {
                detector.reset();
                detector.measure_samples(&tone(kind, frequency, 48_000.0, frames, 0.3));
                let pitch = detector.latest();
                assert!(
                    cents(pitch.frequency, frequency).abs() < 0.1 && pitch.confidence > 0.99,
                    "{method:?} {kind:?} at {frequency} Hz: {pitch:?}"
                );
            }
        }
    }
}

#[test]
fn the_note_and_its_offset_are_named() {
    let mut detector = detector(PitchMethod::yin(), LagInterpolation::BandLimited, 48_000.0);
    let frames = detector.frame_len();
    for (frequency, name, offset) in [
        (440.0, "A4", 0.0),
        (261.63, "C4", 0.02),
        (446.0, "A4", 23.45),
        (1_046.5, "C6", 0.0),
        (82.0, "E2", -8.57),
    ] {
        detector.reset();
        detector.measure_samples(&tone(Kind::Saw, frequency, 48_000.0, frames, 0.3));
        let note = detector.latest().note().unwrap();
        assert_eq!(note.to_string(), name);
        assert!(
            (note.cents() - offset).abs() < 1.0,
            "{frequency}: {} cents",
            note.cents()
        );
    }
}

#[test]
fn extreme_note_arguments_give_an_answer_instead_of_panicking() {
    // `nearest_with_reference` promises `None` only for arguments that are
    // not positive numbers. The ratio of these two overflows to infinity;
    // it used to make `A4_MIDI + steps as i32` overflow too, a panic in
    // debug builds and a wrapped note number in release builds.
    let note = Note::nearest_with_reference(1e300, 1e-300).unwrap();
    assert!(note.cents().is_finite(), "{note:?}");
    assert_eq!(note.midi_number(), 69 + 23_918);
    // `from_midi` takes any i32; `frequency` subtracts 69 from it.
    let frequency = Note::from_midi(i32::MIN).frequency(440.0);
    assert!(frequency.is_finite() && frequency >= 0.0, "{frequency}");
}

#[test]
fn readings_follow_from_another_thread() {
    // A melody of four notes, a quarter of a second each, played on the
    // audio thread in blocks of 128; a reader polls the published
    // estimate. Frequency and confidence are published together, so every
    // reading with a very high confidence is one of the notes: a frame
    // that holds the change of note never reaches it.
    //
    // At the end of each note the audio thread waits until the reader has
    // polled twice more, so that the reader sees every note however the
    // threads are scheduled; a reader that stops or stalls fails the test
    // instead of hanging it.
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    let notes = [220.0, 330.0, 440.0, 550.0];
    let mut detector = detector(PitchMethod::mpm(), LagInterpolation::BandLimited, 48_000.0);
    let readings = detector.readings().clone();
    let mut block = AudioBuffer::<f32>::new(2, 128);
    let done = Arc::new(AtomicBool::new(false));
    let polls = Arc::new(AtomicU64::new(0));
    let reader = {
        let done = Arc::clone(&done);
        let polls = Arc::clone(&polls);
        std::thread::spawn(move || {
            let mut seen = Vec::new();
            while !done.load(Ordering::Acquire) {
                let pitch = readings.pitch();
                if pitch.confidence > 0.99 {
                    seen.push(pitch.frequency);
                }
                polls.fetch_add(1, Ordering::AcqRel);
                std::thread::yield_now();
            }
            (seen, readings.pitch())
        })
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    for frequency in notes {
        let note = tone(Kind::Square, frequency, 48_000.0, 12_000, 0.0);
        for chunk in note.chunks(128) {
            block.set_num_frames(chunk.len());
            // The same melody in both channels: the mean is the melody.
            block.channel_mut(0).copy_from_slice(chunk);
            block.channel_mut(1).copy_from_slice(chunk);
            detector.measure(&block);
        }
        // The first poll counted may have begun before the last block was
        // published; the second began after it.
        let before = polls.load(Ordering::Acquire);
        while polls.load(Ordering::Acquire) < before + 2 {
            assert!(!reader.is_finished(), "the reader stopped");
            assert!(Instant::now() < deadline, "the reader stalled");
            std::thread::yield_now();
        }
    }
    done.store(true, Ordering::Release);
    let (seen, last) = reader.join().unwrap();
    for frequency in &seen {
        assert!(
            notes
                .iter()
                .any(|note| cents(*frequency, *note).abs() < 3.0),
            "the reader saw {frequency} Hz with high confidence"
        );
    }
    // And it saw every note.
    for note in notes {
        assert!(
            seen.iter()
                .any(|&frequency| cents(frequency, note).abs() < 3.0),
            "the reader never saw {note} Hz"
        );
    }
    println!("the reader saw {} confident readings", seen.len());
    assert!(cents(last.frequency, 550.0).abs() < 1.0, "{last:?}");
    assert_eq!(detector.settings().source, Source::Mean);
}
