//! The pitch detector where it is most likely to go wrong: tones between
//! the semitones near its highest frequency, where a peak or a dip of the
//! lag function is only a sample or two wide; low sample rates; tones just
//! below its lowest frequency; second harmonics stronger than the
//! fundamental; noise of several colours; and the limits it applies.
//!
//! Several of these cases were found in review: the detector used to judge
//! each candidate period by the lag function at whole lags, and near 2 kHz,
//! or at low sample rates, it took the octave below with full confidence.
//!
//! Run with `--nocapture` to see the measured figures.

mod common;

use core::f64::consts::PI;

use common::{Noise, ToneKind, harmonic_tone};
use manguebyte_analysis::{LagInterpolation, Pitch, PitchDetector, PitchMethod, PitchSettings};
use manguebyte_dsp::ProcessSpec;
use manguebyte_dsp::fft::{Complex, RealFft};

const METHODS: [PitchMethod; 2] = [PitchMethod::yin(), PitchMethod::mpm()];

/// The tones every grid is tried with.
const KINDS: [ToneKind; 4] = [
    ToneKind::Sine,
    ToneKind::Saw,
    ToneKind::Square,
    ToneKind::MissingFundamental,
];

fn spec(sample_rate: f64) -> ProcessSpec {
    ProcessSpec {
        sample_rate,
        max_block_size: 512,
        num_channels: 1,
    }
}

fn prepared(settings: PitchSettings, sample_rate: f64) -> PitchDetector {
    let mut detector = PitchDetector::new(settings);
    detector.prepare(&spec(sample_rate));
    detector
}

fn cents(measured: f64, expected: f64) -> f64 {
    1_200.0 * (measured / expected).log2()
}

/// What a detector with default settings but for `method` reads after
/// exactly one frame of the signal `make` builds.
fn one_frame(method: PitchMethod, sample_rate: f64, make: impl Fn(usize) -> Vec<f32>) -> Pitch {
    let mut detector = prepared(PitchSettings::new(method), sample_rate);
    let signal = make(detector.frame_len());
    detector.measure_samples(&signal);
    detector.latest()
}

/// What `detector`, started afresh, reads after exactly one frame of `kind`
/// at `frequency`.
fn read(detector: &mut PitchDetector, kind: ToneKind, frequency: f64, phase: f64) -> Pitch {
    let signal = harmonic_tone(
        kind,
        frequency,
        detector.sample_rate(),
        detector.frame_len(),
        phase,
    );
    detector.reset();
    detector.measure_samples(&signal);
    detector.latest()
}

/// What a detector got right and wrong over a set of tones.
#[derive(Debug, Default)]
struct Survey {
    /// The largest error in cents among readings within a semitone.
    worst: f64,
    worst_at: f64,
    /// The lowest confidence of all readings.
    lowest: f64,
    /// Readings more than a semitone off, or missing: (frequency, phase,
    /// reading).
    gross: Vec<(f64, f64, Pitch)>,
}

fn survey(
    detector: &mut PitchDetector,
    kind: ToneKind,
    frequencies: &[f64],
    phases: &[f64],
) -> Survey {
    let mut result = Survey {
        lowest: 1.0,
        ..Survey::default()
    };
    for &frequency in frequencies {
        for &phase in phases {
            let pitch = read(detector, kind, frequency, phase);
            let error = cents(pitch.frequency, frequency).abs();
            result.lowest = result.lowest.min(pitch.confidence);
            // A missing pitch gives an infinite or a NaN error.
            if error < 100.0 {
                if error > result.worst {
                    result.worst = error;
                    result.worst_at = frequency;
                }
            } else {
                result.gross.push((frequency, phase, pitch));
            }
        }
    }
    result
}

/// Every semitone from A1 (55 Hz) to B6 (1975.5 Hz), and 2 kHz.
fn semitones() -> Vec<f64> {
    (0..=62)
        .map(|semitone| 55.0 * 2f64.powf(f64::from(semitone) / 12.0))
        .chain([2_000.0])
        .collect()
}

#[test]
fn the_mcleod_method_finds_a_missing_fundamental_near_2_khz_at_48_khz() {
    // The listed case "a tone with a missing fundamental", at the sample
    // rate the documentation quotes, with the default settings. Between
    // the semitones the key maximum at one period can be so narrow that
    // its whole lags miss the cutoff; judged at whole lags, the method
    // took the one at two periods: 1687 Hz read 843.4 Hz and 1960 Hz read
    // 980.0 Hz, both with a confidence above 0.999.
    let mut failures = Vec::new();
    for frequency in [1_477.0, 1_687.0, 1_813.0, 1_960.0] {
        for phase in [0.0, 0.7] {
            let pitch = one_frame(PitchMethod::mpm(), 48_000.0, |frames| {
                harmonic_tone(
                    ToneKind::MissingFundamental,
                    frequency,
                    48_000.0,
                    frames,
                    phase,
                )
            });
            let error = cents(pitch.frequency, frequency);
            println!(
                "MPM, missing fundamental {frequency} Hz, phase {phase}: {pitch:?}, {error:.2} cents"
            );
            // A missing pitch gives a NaN or an infinite error.
            if error.is_nan() || error.abs() >= 1.0 {
                failures.push((frequency, phase, pitch));
            }
        }
    }
    assert!(failures.is_empty(), "octave errors: {failures:?}");
}

#[test]
fn yin_finds_a_missing_fundamental_near_2_khz_at_44_1_khz() {
    // The same defect in YIN: at whole lags the dip at one period did not
    // reach the threshold, the dip at two periods did, and 1960 Hz read
    // 980.0 Hz with a confidence of 1.0.
    for phase in [0.0, 0.7] {
        let pitch = one_frame(PitchMethod::yin(), 44_100.0, |frames| {
            harmonic_tone(
                ToneKind::MissingFundamental,
                1_960.0,
                44_100.0,
                frames,
                phase,
            )
        });
        let error = cents(pitch.frequency, 1_960.0);
        println!("YIN, missing fundamental 1960 Hz at 44.1 kHz, phase {phase}: {pitch:?}");
        assert!(
            error.abs() < 1.0,
            "phase {phase}: {pitch:?}, {error:.1} cents"
        );
    }
}

#[test]
fn clean_tones_at_low_sample_rates_have_no_octave_errors() {
    // Sample rates from 8 kHz up are in the range the crate documents,
    // and the default limits (50 Hz to 2 kHz) are used as they are. When a
    // period is only a few samples long, the whole-lag values used to
    // decide the threshold and cutoff tests, and both methods jumped an
    // octave down with full confidence, even on a pure sine: at 8 kHz a
    // 1760 Hz sine read 880 Hz (YIN, confidence 0.992; MPM, 1.000); at
    // 16 kHz a 1396.9 Hz sawtooth read 698.4 Hz (MPM); at 22.05 kHz a
    // 1760 Hz sawtooth read 880 Hz (MPM).
    let cases = [
        (PitchMethod::yin(), 8_000.0, ToneKind::Sine, 1_760.0),
        (PitchMethod::mpm(), 8_000.0, ToneKind::Sine, 1_760.0),
        (PitchMethod::mpm(), 8_000.0, ToneKind::Sine, 1_480.0),
        (PitchMethod::mpm(), 16_000.0, ToneKind::Saw, 1_396.9),
        (PitchMethod::mpm(), 22_050.0, ToneKind::Saw, 1_760.0),
    ];
    let mut failures = Vec::new();
    for (method, sample_rate, kind, frequency) in cases {
        let pitch = one_frame(method, sample_rate, |frames| {
            harmonic_tone(kind, frequency, sample_rate, frames, 0.2)
        });
        let error = cents(pitch.frequency, frequency);
        println!(
            "{method:?}, {sample_rate} Hz, {kind:?} {frequency} Hz: {pitch:?}, {error:.2} cents"
        );
        // Generous: this is about octave errors, not about accuracy.
        if error.is_nan() || error.abs() >= 10.0 {
            failures.push((method, sample_rate, kind, frequency, pitch));
        }
    }
    assert!(failures.is_empty(), "octave errors: {failures:?}");
}

#[test]
fn yin_is_not_sure_of_a_tone_just_below_its_lowest_frequency() {
    // The documentation used to say that YIN reports a tone below its
    // lowest frequency "with a low confidence at the longest period", but
    // 48 Hz read 49.95 Hz (69 cents sharp) with a confidence of 0.971 and
    // 49 Hz read it at 0.992. Now such a tone has no pitch at all; see
    // also `only_periods_within_the_limits_are_reported` in pitch.rs.
    for frequency in [47.0, 48.0, 49.0] {
        let pitch = one_frame(PitchMethod::yin(), 48_000.0, |frames| {
            harmonic_tone(ToneKind::Sine, frequency, 48_000.0, frames, 0.3)
        });
        let error = cents(pitch.frequency, frequency);
        println!("YIN, sine {frequency} Hz below the 50 Hz limit: {pitch:?}");
        assert!(
            pitch.confidence < 0.5 || error.abs() < 2.0,
            "{frequency} Hz: {pitch:?}, {error:.1} cents"
        );
        assert_eq!(pitch, Pitch::NONE);
    }
}

#[test]
fn tones_between_the_semitones_near_2_khz_have_no_octave_errors() {
    // Every 7 Hz from 1400 Hz to 2002 Hz, where a period is 22 to 34
    // samples and a peak or a dip of the lag function of a tone rich in
    // harmonics is about two samples wide.
    let frequencies: Vec<f64> = (0..=86)
        .map(|step| 1_400.0 + 7.0 * f64::from(step))
        .collect();
    for sample_rate in [44_100.0, 48_000.0] {
        for method in METHODS {
            let mut detector = prepared(PitchSettings::new(method), sample_rate);
            for kind in KINDS {
                let result = survey(&mut detector, kind, &frequencies, &[0.0, 0.4]);
                println!(
                    "{sample_rate} Hz, {method:?}, {kind:?}: worst {:.3} cents at {} Hz, lowest confidence {:.3}",
                    result.worst, result.worst_at, result.lowest
                );
                assert!(
                    result.gross.is_empty(),
                    "{sample_rate} {method:?} {kind:?}: {:?}",
                    result.gross
                );
                // Within two cents: harmonics close to half the sample
                // rate are the hardest to interpolate (see the
                // documentation's accuracy section).
                let allowed = if kind == ToneKind::Sine { 0.1 } else { 2.0 };
                assert!(
                    result.worst < allowed,
                    "{sample_rate} {method:?} {kind:?}: {} cents",
                    result.worst
                );
                assert!(result.lowest > 0.98, "{}", result.lowest);
            }
        }
    }
    // The parabola places the period less well, but the period it places
    // is chosen the same way: no octave errors either, and within the
    // 5.3 cents the documentation states.
    for sample_rate in [44_100.0, 48_000.0] {
        for method in METHODS {
            let settings =
                PitchSettings::new(method).with_interpolation(LagInterpolation::Parabolic);
            let mut detector = prepared(settings, sample_rate);
            for kind in KINDS {
                let result = survey(&mut detector, kind, &frequencies, &[0.4]);
                println!(
                    "{sample_rate} Hz, {method:?}, parabolic, {kind:?}: worst {:.3} cents at {} Hz",
                    result.worst, result.worst_at
                );
                assert!(result.gross.is_empty(), "{:?}", result.gross);
                assert!(result.worst < 5.5, "{}", result.worst);
            }
        }
    }
}

#[test]
fn a_random_grid_from_55_hz_to_2_khz_has_no_octave_errors() {
    // Frequencies spread evenly on a logarithmic scale, at random.
    let mut random = Noise(23);
    let frequencies: Vec<f64> = (0..60)
        .map(|_| 55.0 * (2_000.0f64 / 55.0).powf(0.5 + 0.5 * random.next()))
        .collect();
    let kinds = [
        ToneKind::Sine,
        ToneKind::Saw,
        ToneKind::Square,
        ToneKind::MissingFundamental,
        ToneKind::StrongSecond(2.0),
        ToneKind::StrongSecond(3.0),
    ];
    for sample_rate in [44_100.0, 48_000.0] {
        for method in METHODS {
            let mut detector = prepared(PitchSettings::new(method), sample_rate);
            for kind in kinds {
                let result = survey(&mut detector, kind, &frequencies, &[0.3]);
                println!(
                    "{sample_rate} Hz, {method:?}, {kind:?}: worst {:.3} cents at {:.1} Hz, lowest confidence {:.3}",
                    result.worst, result.worst_at, result.lowest
                );
                assert!(result.gross.is_empty(), "{:?}", result.gross);
                let allowed = if kind == ToneKind::Sine { 0.2 } else { 2.0 };
                assert!(result.worst < allowed, "{}", result.worst);
                assert!(result.lowest > 0.98, "{}", result.lowest);
            }
        }
    }
}

#[test]
fn low_sample_rates_have_no_octave_errors() {
    // The default limits, 50 Hz to 2 kHz, at 8, 16 and 22.05 kHz: the
    // shortest period is 4, 8 and 11 samples. Every semitone, and every
    // 10 Hz from 1400 Hz up.
    let frequencies: Vec<f64> = semitones()
        .into_iter()
        .chain((0..=60).map(|step| 1_400.0 + 10.0 * f64::from(step)))
        .collect();
    // Just above the largest errors measured, in cents, which the
    // documentation states: 13.3, 5.7 and 3.5, and for sines 1.3, 0.2 and
    // 0.2.
    for (sample_rate, rich, sines) in [
        (8_000.0, 14.0, 1.4),
        (16_000.0, 6.0, 0.2),
        (22_050.0, 4.0, 0.2),
    ] {
        for method in METHODS {
            let mut detector = prepared(PitchSettings::new(method), sample_rate);
            for kind in KINDS {
                let allowed = if kind == ToneKind::Sine { sines } else { rich };
                // A tone without its fundamental that keeps a single
                // harmonic below half the sample rate (at 8 kHz, above
                // 1333 Hz) is a lone sine near half the rate, which says
                // nothing of the fundamental: left out.
                let usable: Vec<f64> = frequencies
                    .iter()
                    .copied()
                    .filter(|&frequency| {
                        kind != ToneKind::MissingFundamental
                            || kind.harmonics_present(frequency, sample_rate) >= 2
                    })
                    .collect();
                let result = survey(&mut detector, kind, &usable, &[0.0, 0.7]);
                println!(
                    "{sample_rate} Hz, {method:?}, {kind:?}: worst {:.3} cents at {:.1} Hz, lowest confidence {:.3}",
                    result.worst, result.worst_at, result.lowest
                );
                assert!(
                    result.gross.is_empty(),
                    "{sample_rate} {method:?} {kind:?}: {:?}",
                    result.gross
                );
                assert!(
                    result.worst < allowed,
                    "{sample_rate} {method:?} {kind:?}: {} cents",
                    result.worst
                );
                assert!(result.lowest > 0.97, "{}", result.lowest);
            }
        }
    }
}

#[test]
fn second_harmonics_stronger_than_the_documented_limit_take_the_octave_above() {
    // With a second harmonic r times as strong as the fundamental, half
    // the period is nearly a period. YIN's normalised difference there is
    // 2 / (1 + r^2), which falls below the threshold t beyond
    // r = sqrt(2 / t - 1); the McLeod method's normalised square
    // difference there is (r^2 - 1) / (r^2 + 1), which reaches the cutoff
    // c beyond r = sqrt((1 + c) / (1 - c)). The detector takes the octave
    // above beyond those limits, and not short of them; tried with the
    // default threshold and cutoff and one other each.
    let frequencies: Vec<f64> = semitones().into_iter().step_by(2).collect();
    for method in [
        PitchMethod::yin(),
        PitchMethod::Yin { threshold: 0.3 },
        PitchMethod::mpm(),
        PitchMethod::Mpm { cutoff: 0.8 },
    ] {
        let limit = match method {
            PitchMethod::Yin { threshold } => (2.0 / threshold - 1.0).sqrt(),
            PitchMethod::Mpm { cutoff } => ((1.0 + cutoff) / (1.0 - cutoff)).sqrt(),
            _ => unreachable!(),
        };
        let mut detector = prepared(PitchSettings::new(method), 48_000.0);
        for (ratio, above) in [(0.93 * limit, false), (1.1 * limit, true)] {
            let mut octaves = 0;
            for &frequency in &frequencies {
                let pitch = read(&mut detector, ToneKind::StrongSecond(ratio), frequency, 0.0);
                // The octave above is a pitch only if it is within the
                // limits, 2 kHz at the most.
                let expected = if above && 2.0 * frequency <= 2_000.0 {
                    octaves += 1;
                    2.0 * frequency
                } else {
                    frequency
                };
                let error = cents(pitch.frequency, expected);
                // The octave above is not a true period, so it is placed
                // less exactly.
                let allowed = if expected == frequency { 1.0 } else { 50.0 };
                assert!(
                    error.abs() < allowed,
                    "{method:?}, r = {ratio:.3}, {frequency:.1} Hz: {pitch:?}, {error:.1} cents from {expected:.1} Hz"
                );
            }
            println!(
                "{method:?}: limit r = {limit:.3} ({:.1} dB); at r = {ratio:.3}, {octaves} of {} tones read an octave up",
                20.0 * limit.log10(),
                frequencies.len()
            );
        }
    }
}

/// Noise that repeats every `len` samples, made in the frequency domain:
/// every bin from 20 Hz to below half the sample rate has a random phase
/// and the magnitude `shape(frequency)`, which sets its colour. Scaled to a
/// peak of 0.9.
fn coloured_noise(len: usize, sample_rate: f64, seed: u64, shape: impl Fn(f64) -> f64) -> Vec<f32> {
    let mut spectrum = vec![Complex::<f64>::ZERO; len / 2 + 1];
    let mut random = Noise(seed);
    for (bin, value) in spectrum.iter_mut().enumerate().take(len / 2) {
        let frequency = bin as f64 * sample_rate / len as f64;
        let phase = PI * random.next();
        if frequency >= 20.0 {
            *value = Complex::from_polar(shape(frequency), phase);
        }
    }
    let mut signal = vec![0.0; len];
    RealFft::new(len).inverse(&spectrum, &mut signal);
    let peak = signal
        .iter()
        .fold(0.0f64, |peak, value| peak.max(value.abs()));
    signal
        .iter()
        .map(|value| (0.9 * value / peak) as f32)
        .collect()
}

#[test]
fn noise_of_every_colour_reads_no_higher_than_the_documentation_says() {
    // 1.4 s of each, read after every analysis, all from 20 Hz up. Noise
    // whose power lies at low frequencies changes slowly, and over a frame
    // it can look nearly periodic. Measured, the highest confidence with
    // YIN and with the McLeod method: white 0.15 and 0.13, pink 0.38 and
    // 0.33, white low-passed at 200 Hz 0.63 and 0.54, brown 0.69 and 0.68.
    // The documentation states these, and the test holds them to it.
    let rate = 48_000.0;
    // Name, magnitude against frequency, highest confidence allowed.
    type Colour = (&'static str, fn(f64) -> f64, f64);
    let colours: [Colour; 4] = [
        ("white", |_| 1.0, 0.2),
        ("pink", |frequency| 1.0 / frequency.sqrt(), 0.4),
        (
            "white low-passed at 200 Hz",
            |frequency| 1.0 / (1.0 + (frequency / 200.0).powi(2)).sqrt(),
            0.65,
        ),
        ("brown", |frequency| 1.0 / frequency, 0.7),
    ];
    for (name, shape, allowed) in colours {
        let signal = coloured_noise(1 << 16, rate, 31, shape);
        for method in METHODS {
            let mut detector = prepared(PitchSettings::new(method), rate);
            let mut highest = 0.0f64;
            let mut above_half = 0;
            let mut analyses = 0;
            for chunk in signal.chunks(256) {
                detector.measure_samples(chunk);
                if detector.analysis_count() > 0 {
                    let confidence = detector.latest().confidence;
                    highest = highest.max(confidence);
                    above_half += usize::from(confidence > 0.5);
                    analyses += 1;
                }
            }
            println!(
                "{method:?}, {name} noise: highest confidence {highest:.3}, above 0.5 in {above_half} of {analyses} analyses"
            );
            assert!(analyses > 240);
            assert!(highest < allowed, "{method:?}, {name}: {highest}");
        }
    }
}

#[test]
fn the_limits_in_use_are_reported() {
    // As asked for at 48 kHz: 2 kHz is a period of exactly 24 samples and
    // 50 Hz one of exactly 960.
    let detector = prepared(PitchSettings::default(), 48_000.0);
    assert_eq!(detector.frequency_range(), (50.0, 2_000.0));
    // At 10 MHz the longest period, 131072 samples, is 76.3 Hz: the lower
    // limit is raised, and the settings still say what was asked for.
    let detector = prepared(PitchSettings::default(), 10_000_000.0);
    let (lowest, highest) = detector.frequency_range();
    assert!((lowest - 10_000_000.0 / 131_072.0).abs() < 1e-9, "{lowest}");
    assert_eq!(highest, 2_000.0);
    assert_eq!(detector.settings().min_frequency, 50.0);
    assert_eq!(detector.period_range(), (5_000, 131_072));
    // The same at 48 kHz for a limit below 0.37 Hz, and an upper limit
    // above half the sample rate comes down to it.
    let settings = PitchSettings::default().with_range(0.1, 30_000.0);
    let detector = prepared(settings, 48_000.0);
    assert_eq!(detector.frequency_range(), (48_000.0 / 131_072.0, 24_000.0));
    // Periods are whole samples, so the range only grows: 44.1 kHz / 2 kHz
    // is 22.05 samples, searched from 22.
    let detector = prepared(PitchSettings::default(), 44_100.0);
    let (lowest, highest) = detector.frequency_range();
    assert_eq!(lowest, 50.0);
    assert!(highest >= 2_000.0 && (highest - 44_100.0 / 22.0).abs() < 1e-9);
}
