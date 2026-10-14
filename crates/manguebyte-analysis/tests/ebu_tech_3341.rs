//! The "minimum requirements test signals" of EBU Tech 3341 (version 4,
//! November 2023), Table 1.
//!
//! The EBU publishes the signals as files. Here each one is synthesised
//! from its description in the table, so nothing has to be downloaded or
//! stored, and every case is checked against the response and tolerance
//! the table gives.
//!
//! Cases 7 and 8 are left out. They are recordings ("authentic programme",
//! one similar to a commercial and one to a film) and cannot be made from a
//! description.
//!
//! Run with `--nocapture` to see what each case read.

mod common;

use common::{Tone, both, db, fade, spec, stereo};
use manguebyte_analysis::{LoudnessMeter, TruePeakMeter};
use manguebyte_dsp::fir;

const RATE: f64 = 48_000.0;

/// What the table allows for every loudness reading, in LU.
const TOLERANCE: f64 = 0.1;

fn stereo_meter() -> LoudnessMeter {
    let mut meter = LoudnessMeter::stereo();
    meter.prepare(&spec(RATE, 2));
    meter
}

/// The 1 kHz stereo tone of test case 1, "applied in phase to both channels
/// simultaneously".
fn tone() -> Tone {
    Tone::new(RATE, 1_000.0, 2)
}

/// Plays segments of (level in dBFS or silence, seconds) into `meter`.
fn play(meter: &mut LoudnessMeter, tone: &mut Tone, segments: &[(Option<f64>, f64)]) {
    for &(level, seconds) in segments {
        tone.play(&[level, level], seconds, |block| meter.measure(block));
    }
}

/// Checks a reading against the table and prints it.
#[track_caller]
fn check(case: &str, reading: f64, expected: f64) {
    println!("Tech 3341 case {case}: {reading:.3} LUFS (expected {expected:.1} +-{TOLERANCE})");
    assert!(
        (reading - expected).abs() <= TOLERANCE,
        "case {case}: {reading:.3} LUFS, expected {expected:.1} within {TOLERANCE}"
    );
}

#[test]
fn cases_1_and_2_a_steady_tone_reads_its_level_on_all_three_scales() {
    // "Stereo sine wave, 1000 Hz, -23.0 dBFS (per-channel peak level);
    // signal applied in phase to both channels simultaneous; 20 s
    // duration", and the same at -33.0 dBFS.
    for (case, level) in [("1", -23.0), ("2", -33.0)] {
        let mut meter = stereo_meter();
        let readings = meter.readings().clone();
        let mut blocks = 0;
        // Not only the last reading: every one on the way, from the moment
        // each window is full.
        let (mut worst_momentary, mut worst_short_term, mut worst_integrated) =
            (0.0f64, 0.0f64, 0.0f64);
        tone().play(&both(level), 20.0, |block| {
            meter.measure(block);
            blocks += 1;
            if blocks >= 40 {
                worst_momentary = worst_momentary.max((readings.momentary() - level).abs());
                worst_integrated = worst_integrated.max((readings.integrated() - level).abs());
            }
            if blocks >= 300 {
                worst_short_term = worst_short_term.max((readings.short_term() - level).abs());
            }
        });
        assert!(worst_momentary <= TOLERANCE, "M strays {worst_momentary}");
        assert!(worst_short_term <= TOLERANCE, "S strays {worst_short_term}");
        assert!(worst_integrated <= TOLERANCE, "I strays {worst_integrated}");
        check(&format!("{case} M"), readings.momentary(), level);
        check(&format!("{case} S"), readings.short_term(), level);
        check(&format!("{case} I"), readings.integrated(), level);
        // On the relative scale: 0.0 LU and -10.0 LU.
        let relative = readings.integrated() - manguebyte_analysis::EBU_R128_TARGET_LUFS;
        assert!((relative - (level + 23.0)).abs() <= TOLERANCE);
    }
}

#[test]
fn case_3_the_relative_gate_drops_the_quiet_ends() {
    // "10 s at -36.0 dBFS; 60 s at -23.0 dBFS; 10 s at -36.0 dBFS"
    let mut meter = stereo_meter();
    play(
        &mut meter,
        &mut tone(),
        &[
            (Some(-36.0), 10.0),
            (Some(-23.0), 60.0),
            (Some(-36.0), 10.0),
        ],
    );
    check("3 I", meter.readings().integrated(), -23.0);
}

#[test]
fn case_4_the_absolute_gate_drops_what_is_below_minus_70() {
    // "10 s at -72.0 dBFS; 10 s at -36.0 dBFS; 60 s at -23.0 dBFS; 10 s at
    // -36.0 dBFS; 10 s at -72.0 dBFS"
    let mut meter = stereo_meter();
    play(
        &mut meter,
        &mut tone(),
        &[
            (Some(-72.0), 10.0),
            (Some(-36.0), 10.0),
            (Some(-23.0), 60.0),
            (Some(-36.0), 10.0),
            (Some(-72.0), 10.0),
        ],
    );
    check("4 I", meter.readings().integrated(), -23.0);
}

#[test]
fn case_5_levels_inside_the_relative_gate_are_all_counted() {
    // "20 s at -26.0 dBFS; 20.1 s at -20.0 dBFS; 20 s at -26.0 dBFS"
    let mut meter = stereo_meter();
    play(
        &mut meter,
        &mut tone(),
        &[
            (Some(-26.0), 20.0),
            (Some(-20.0), 20.1),
            (Some(-26.0), 20.0),
        ],
    );
    check("5 I", meter.readings().integrated(), -23.0);
}

#[test]
fn case_6_five_channels_are_weighted() {
    // "5.0 channel sine wave, 1000 Hz, 20 s duration, with per-channel
    // peak levels as follows: -28.0 dBFS in L and R, -24.0 dBFS in C,
    // -30.0 dBFS in Ls and Rs"
    let mut meter = LoudnessMeter::surround_5_0();
    meter.prepare(&spec(RATE, 5));
    let levels = [
        Some(-28.0),
        Some(-28.0),
        Some(-24.0),
        Some(-30.0),
        Some(-30.0),
    ];
    Tone::new(RATE, 1_000.0, 5).play(&levels, 20.0, |block| meter.measure(block));
    check("6 I", meter.readings().integrated(), -23.0);

    // The same programme as a 5.1 file, with the loudest tone of all in
    // the channel that is not measured.
    let mut meter = LoudnessMeter::surround_5_1();
    meter.prepare(&spec(RATE, 6));
    let levels = [
        Some(-28.0),
        Some(-28.0),
        Some(-24.0),
        Some(0.0),
        Some(-30.0),
        Some(-30.0),
    ];
    Tone::new(RATE, 1_000.0, 6).play(&levels, 20.0, |block| meter.measure(block));
    check("6 I, with an LFE", meter.readings().integrated(), -23.0);
}

#[test]
fn case_9_short_term_loudness_is_steady_over_a_repeating_pattern() {
    // "(1.34 s at -20.0 dBFS; 1.66 s at -30.0 dBFS) repeated 5 times":
    // "S = -23.0 +-0.1 LUFS, constant after 3 s". Any three seconds hold
    // exactly one period of the pattern.
    let mut meter = stereo_meter();
    let readings = meter.readings().clone();
    let mut tone = tone();
    let mut blocks = 0;
    let (mut lowest, mut highest) = (f64::MAX, f64::MIN);
    for _ in 0..5 {
        for (level, seconds) in [(-20.0, 1.34), (-30.0, 1.66)] {
            tone.play(&both(level), seconds, |block| {
                meter.measure(block);
                blocks += 1;
                if blocks >= 300 {
                    lowest = lowest.min(readings.short_term());
                    highest = highest.max(readings.short_term());
                }
            });
        }
    }
    check("9 S, lowest", lowest, -23.0);
    check("9 S, highest", highest, -23.0);
}

#[test]
fn case_10_the_short_term_window_slides() {
    // "For file-based meters; 20 segments ... (i * 0.15 s of silence; 3 s
    // at -23.0 dBFS; 1 s of silence) for i = 0, 1, 2, ..., 19": "Max S =
    // -23.0 +-0.1 LUFS, for each segment". A window that only moved every
    // 3 s, or every second, would catch the tone whole in few of them.
    for i in 0..20 {
        let mut meter = stereo_meter();
        play(
            &mut meter,
            &mut tone(),
            &[(None, f64::from(i) * 0.15), (Some(-23.0), 3.0), (None, 1.0)],
        );
        check(
            &format!("10.{i} max S"),
            meter.readings().max_short_term(),
            -23.0,
        );
    }
}

#[test]
fn case_11_the_maximum_short_term_loudness_follows_rising_tones() {
    // "For 'live' meters; 20 tones ... (i * 0.15 s of silence; 3 s at
    // -38.0+i dBFS; 3 - i * 0.15 s of silence) for i = 0, 1, 2, ..., 19":
    // "Max S = -38.0, -37.0, -36.0, ..., -19.0 +-0.1 LUFS, successive
    // values". One meter, never reset.
    let mut meter = stereo_meter();
    let mut tone = tone();
    for i in 0..20 {
        let offset = f64::from(i) * 0.15;
        let level = -38.0 + f64::from(i);
        play(
            &mut meter,
            &mut tone,
            &[(None, offset), (Some(level), 3.0), (None, 3.0 - offset)],
        );
        check(
            &format!("11.{i} max S"),
            meter.readings().max_short_term(),
            level,
        );
    }
}

#[test]
fn case_12_momentary_loudness_is_steady_over_a_repeating_pattern() {
    // "(0.18 s at -20.0 dBFS; 0.22 s at -30.0 dBFS) repeated 25 times":
    // "M = -23.0 +-0.1 LUFS, constant after 1 s".
    let mut meter = stereo_meter();
    let readings = meter.readings().clone();
    let mut tone = tone();
    let mut blocks = 0;
    let (mut lowest, mut highest) = (f64::MAX, f64::MIN);
    for _ in 0..25 {
        for (level, seconds) in [(-20.0, 0.18), (-30.0, 0.22)] {
            tone.play(&both(level), seconds, |block| {
                meter.measure(block);
                blocks += 1;
                if blocks >= 100 {
                    lowest = lowest.min(readings.momentary());
                    highest = highest.max(readings.momentary());
                }
            });
        }
    }
    check("12 M, lowest", lowest, -23.0);
    check("12 M, highest", highest, -23.0);
}

#[test]
fn case_13_the_momentary_window_slides() {
    // "For file-based meters; 20 segments ... (i * 20 ms of silence;
    // 400 ms at -23.0 dBFS; 1 s of silence) for i = 0, 1, 2, ..., 19":
    // "Max M = -23.0 +-0.1 LUFS, for each segment". A meter that only
    // looked every 100 ms would read up to 0.46 LU low here.
    for i in 0..20 {
        let mut meter = stereo_meter();
        play(
            &mut meter,
            &mut tone(),
            &[(None, f64::from(i) * 0.02), (Some(-23.0), 0.4), (None, 1.0)],
        );
        check(
            &format!("13.{i} max M"),
            meter.readings().max_momentary(),
            -23.0,
        );
    }
}

#[test]
fn case_14_the_maximum_momentary_loudness_follows_rising_tones() {
    // "For 'live' meters; 20 tones ... (i * 20 ms of silence; 400 ms at
    // -38.0+i dBFS; 400 - i * 20 ms of silence) for i = 0, 1, 2, ..., 19":
    // "Max M = -38.0, -37.0, -36.0, ..., -19.0 +-0.1 LUFS, successive
    // values". One meter, never reset.
    let mut meter = stereo_meter();
    let mut tone = tone();
    for i in 0..20 {
        let offset = f64::from(i) * 0.02;
        let level = -38.0 + f64::from(i);
        play(
            &mut meter,
            &mut tone,
            &[(None, offset), (Some(level), 0.4), (None, 0.4 - offset)],
        );
        check(
            &format!("14.{i} max M"),
            meter.readings().max_momentary(),
            level,
        );
    }
}

/// The maximum true-peak level of `signal` in both channels, in dBTP, as
/// the default meter reads it. The two channels must agree.
fn true_peak_db(signal: &[f64], sample_rate: f64) -> f64 {
    let mut meter = TruePeakMeter::new(2);
    meter.prepare(&spec(sample_rate, 2));
    meter.measure(&stereo(signal));
    // The signal is a whole file: its end is read too.
    meter.flush();
    let readings = meter.readings();
    assert_eq!(readings.max(0), readings.max(1));
    readings.max_overall_db()
}

/// Checks a true-peak reading against the table and prints it. The
/// tolerance is the table's: 0.2 dB over, 0.4 dB under.
#[track_caller]
fn check_true_peak(case: &str, reading: f64, expected: f64) {
    println!("Tech 3341 case {case}: {reading:.3} dBTP (expected {expected:.1} +0.2/-0.4)");
    assert!(
        (expected - 0.4..=expected + 0.2).contains(&reading),
        "case {case}: {reading:.3} dBTP, expected {expected:.1} +0.2/-0.4"
    );
}

#[test]
fn cases_15_to_19_a_sine_is_read_at_its_crest_wherever_the_samples_fall() {
    // "Stereo sine wave with frequency fs/4 Hz, amplitude 0.50 FFS, phase
    // 0.0 degrees. ... The duration of the synthesized tone does not
    // matter, but the tone should be tapered with a 10 ms fade-in and
    // fade-out." Then the same at 45 degrees, at fs/6 and 60 degrees, at
    // fs/8 and 67.5 degrees, and at fs/4, 45 degrees and 1.41 FFS.
    // Case, fs divided by this, amplitude, phase in degrees, expected dBTP.
    let cases: [(&str, f64, f64, f64, f64); 5] = [
        ("15", 4.0, 0.50, 0.0, -6.0),
        ("16", 4.0, 0.50, 45.0, -6.0),
        ("17", 6.0, 0.50, 60.0, -6.0),
        ("18", 8.0, 0.50, 67.5, -6.0),
        ("19", 4.0, 1.41, 45.0, 3.0),
    ];
    // "The frequency fs/4 denotes 12 kHz for a sample-rate of 48 kHz,
    // etc.": the cases are defined for any rate, so they are run at three.
    for sample_rate in [48_000.0, 44_100.0, 96_000.0] {
        for (case, divisor, amplitude, degrees, expected) in cases {
            let frames = (0.5 * sample_rate) as usize;
            let mut signal: Vec<f64> = (0..frames)
                .map(|n| {
                    let phase = core::f64::consts::TAU * n as f64 / divisor + degrees.to_radians();
                    amplitude * phase.sin()
                })
                .collect();
            fade(&mut signal, (0.01 * sample_rate) as usize);
            let sample_peak = db(signal.iter().fold(0.0f64, |peak, s| peak.max(s.abs())));
            let reading = true_peak_db(&signal, sample_rate);
            check_true_peak(&format!("{case} at {sample_rate} Hz"), reading, expected);
            println!("  (the largest sample is at {sample_peak:.3} dBFS)");
        }
    }
}

#[test]
fn cases_20_to_23_a_peak_between_the_samples_is_found_at_every_offset() {
    // "Stereo sine wave with frequency fs/6 Hz, amplitude 0.50 FFS,
    // containing a single period of a sine wave with frequency fs/4,
    // amplitude 1.00; the signal being continuous in phase at both sides
    // of the single period. The signal is synthesized at 4*fs (for
    // example, 192 kHz), and then lowpass (anti-aliasing) filtered and
    // downsampled to fs with a 0 samples offset", and then with offsets of
    // 1, 2 and 3 samples: "Max. true-peak level = 0.0 +0.2/-0.4 dBTP".
    //
    // At four times the rate a period of fs/6 is 24 samples and one of
    // fs/4 is 16.
    let mut fast: Vec<f64> = Vec::new();
    let carrier =
        |frames: usize| (0..frames).map(|n| 0.5 * (core::f64::consts::TAU * n as f64 / 24.0).sin());
    // 300 whole periods, so the tone arrives at the single period with a
    // phase of zero, and leaves it the same way.
    fast.extend(carrier(24 * 300));
    fast.extend((0..16).map(|n| (core::f64::consts::TAU * f64::from(n) / 16.0).sin()));
    fast.extend(carrier(24 * 300));
    fade(&mut fast, 4 * 480);
    assert!((db(fast.iter().fold(0.0f64, |peak, s| peak.max(s.abs())))).abs() < 1e-9);

    // The table does not say which low-pass. This one is flat up to 0.47
    // of the lower rate and 96 dB down from 0.53 of it.
    let lowpass = fir::lowpass(511, 0.125, 96.0);
    let filtered: Vec<f64> = (0..fast.len() + lowpass.len() - 1)
        .map(|n| {
            lowpass
                .iter()
                .enumerate()
                .filter(|&(tap, _)| n >= tap && n - tap < fast.len())
                .map(|(tap, &coefficient)| coefficient * fast[n - tap])
                .sum()
        })
        .collect();
    // What the filter left of the peak, seen at four times the rate: the
    // level that each of the four downsampled files ought to read.
    let truth = db(filtered.iter().fold(0.0f64, |peak, s| peak.max(s.abs())));
    println!("Tech 3341 cases 20-23: the filtered peak is {truth:.3} dBFS at 4 fs");
    assert!((-0.4..=0.2).contains(&truth), "{truth}");

    for offset in 0..4 {
        let slow: Vec<f64> = filtered.iter().skip(offset).step_by(4).copied().collect();
        let sample_peak = db(slow.iter().fold(0.0f64, |peak, s| peak.max(s.abs())));
        let reading = true_peak_db(&slow, RATE);
        check_true_peak(&format!("{}", 20 + offset), reading, 0.0);
        println!("  (the largest sample is at {sample_peak:.3} dBFS)");
        // Closer than the table asks: within 0.01 dB of the filtered peak,
        // whichever of the four sets of samples was kept.
        assert!((reading - truth).abs() < 0.01, "{reading} vs {truth}");
    }
}
