//! The "minimum requirements test signals" for loudness range of EBU Tech
//! 3342 (version 4, November 2023), Table 1.
//!
//! As with the Tech 3341 signals, each one is synthesised from its
//! description. Cases 5 and 6 are left out: they are recordings
//! ("authentic programme") and cannot be made from a description.
//!
//! Run with `--nocapture` to see what each case read.

mod common;

use common::{Tone, both, spec};
use manguebyte_analysis::LoudnessMeter;

const RATE: f64 = 48_000.0;

/// What the table allows, in LU.
const TOLERANCE: f64 = 1.0;

/// The loudness range of a stereo 1 kHz tone played at each of `levels`
/// (dBFS) for 20 seconds, the whole sequence `repeats` times over.
fn range_of(levels: &[f64], repeats: usize) -> f64 {
    let mut meter = LoudnessMeter::stereo();
    meter.prepare(&spec(RATE, 2));
    let mut tone = Tone::new(RATE, 1_000.0, 2);
    for _ in 0..repeats {
        for &level in levels {
            tone.play(&both(level), 20.0, |block| meter.measure(block));
        }
    }
    meter.readings().range()
}

#[track_caller]
fn check(case: &str, levels: &[f64], expected: f64) {
    let once = range_of(levels, 1);
    println!("Tech 3342 case {case}: LRA {once:.2} LU (expected {expected:.0} +-{TOLERANCE})");
    assert!(
        (once - expected).abs() <= TOLERANCE,
        "case {case}: {once:.2} LU, expected {expected:.0} within {TOLERANCE}"
    );
    // "In all the above test cases, the expected response is unchanged if
    // the test signal is repeated one or more times in its full length."
    let twice = range_of(levels, 2);
    println!("Tech 3342 case {case}, played twice: LRA {twice:.2} LU");
    assert!(
        (twice - expected).abs() <= TOLERANCE,
        "case {case} twice: {twice:.2} LU, expected {expected:.0} within {TOLERANCE}"
    );
}

#[test]
fn case_1_two_tones_ten_decibels_apart() {
    // "Stereo sine wave, 1000 Hz, -20.0 dBFS (per-channel peak level);
    // signal applied in phase to both channels simultaneous, 20 s
    // duration; followed immediately by the same signal at -30.0 dBFS"
    check("1", &[-20.0, -30.0], 10.0);
}

#[test]
fn case_2_two_tones_five_decibels_apart() {
    // "As #1, with the 2 tones at -20.0 dBFS and -15.0 dBFS, respectively"
    check("2", &[-20.0, -15.0], 5.0);
}

#[test]
fn case_3_two_tones_twenty_decibels_apart() {
    // "As #1, with the 2 tones at -40.0 dBFS and -20.0 dBFS, respectively"
    check("3", &[-40.0, -20.0], 20.0);
}

#[test]
fn case_4_the_relative_gate_drops_the_quietest_tones() {
    // "As #1, but with 5 tone-segments at -50.0 dBFS, -35.0 dBFS,
    // -20.0 dBFS, -35.0 dBFS, and -50.0 dBFS, respectively; 20 s duration
    // each tone". The two at -50 are more than 20 LU under the level of
    // the whole and must not count, which leaves 15 LU and not 30.
    check("4", &[-50.0, -35.0, -20.0, -35.0, -50.0], 15.0);
}

#[test]
fn the_range_is_not_stable_at_once_and_says_how_long_it_has_measured() {
    // Tech 3341 asks a meter to show that the range is not yet stable
    // during the first 60 s. The meter reports the time for that.
    let mut meter = LoudnessMeter::stereo();
    meter.prepare(&spec(RATE, 2));
    let mut tone = Tone::new(RATE, 1_000.0, 2);
    tone.play(&both(-20.0), 2.99, |block| meter.measure(block));
    assert_eq!(meter.readings().range(), 0.0);
    assert_eq!(meter.readings().measured_seconds(), 2.99);
    tone.play(&both(-20.0), 57.01, |block| meter.measure(block));
    assert!(meter.readings().range() < 0.02);
    assert_eq!(meter.readings().measured_seconds(), 60.0);
}
