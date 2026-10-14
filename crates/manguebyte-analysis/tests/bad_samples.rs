//! What the meters do with samples that are not audio.
//!
//! A filter that blows up upstream hands whatever follows it NaNs,
//! infinities and numbers too large to square. The crate promises that no
//! such sample leaves a meter stuck: one that is not finite is measured as
//! silence, and a finite one beyond 180 dB over full scale as 180 dB over.
//! These tests give every meter every kind of bad sample, in single and in
//! double precision, and require the readings to be back on the programme
//! afterwards, without a reset.
//!
//! The last test measures how long each meter needs to come back from a
//! sample at the limit with its default settings, which is what the
//! documentation of each meter quotes.
//!
//! Run with `--nocapture` to see the readings.

mod common;

use core::f64::consts::TAU;

use common::spec;
use manguebyte_analysis::{
    LoudnessMeter, OctaveFraction, PeakMeter, PitchDetector, PitchMethod, PitchSettings, PpmMeter,
    RmsMeter, SpectrumAnalyzer, SpectrumSettings, StereoMeter, TruePeakMeter, VuMeter,
};
use manguebyte_core::{AudioBuffer, Sample};
use manguebyte_dsp::window::Window;

const RATE: f64 = 48_000.0;

/// The largest magnitude a meter measures: 180 dB over full scale.
const LIMIT: f64 = 1e9;

/// What a 1 kHz sine at -20 dBFS in both channels of a stereo pair reads
/// on the loudness meter, in LUFS: its level, plus the 0.007 dB by which
/// the K-weighting at 1 kHz exceeds the constant of the loudness formula.
const TONE_LUFS: f64 = -19.993;

/// What the tests need of a meter: to take a block of either precision.
trait Meter {
    fn take_single(&mut self, block: &AudioBuffer<f32>);
    fn take_double(&mut self, block: &AudioBuffer<f64>);
}

macro_rules! meters {
    ($($meter:ty),*) => {
        $(impl Meter for $meter {
            fn take_single(&mut self, block: &AudioBuffer<f32>) {
                self.measure(block);
            }

            fn take_double(&mut self, block: &AudioBuffer<f64>) {
                self.measure(block);
            }
        })*
    };
}

meters!(
    LoudnessMeter,
    TruePeakMeter,
    PeakMeter,
    RmsMeter,
    VuMeter,
    PpmMeter,
    StereoMeter,
    SpectrumAnalyzer,
    PitchDetector
);

/// A sample that is not audio, in the precision it arrives in.
#[derive(Debug, Clone, Copy)]
enum Bad {
    Single(f32),
    Double(f64),
}

impl Bad {
    /// Every kind of bad sample: not a number, infinite, and finite but
    /// beyond the limit. The first five of the finite ones in double
    /// precision have a square that is an infinity. The last two, and the
    /// two in single precision, square without trouble and would simply be
    /// read as what they are: 600 dB over full scale for `-1e30`.
    fn all() -> Vec<Bad> {
        let double = [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::MAX,
            f64::MIN,
            1e200,
            -1e200,
            1e155,
            -1e30,
            1.5e9,
        ];
        let single = [
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::MAX,
            f32::MIN,
        ];
        double
            .into_iter()
            .map(Bad::Double)
            .chain(single.into_iter().map(Bad::Single))
            .collect()
    }

    /// The magnitude the meters are documented to measure this sample as.
    fn measured_as(self) -> f64 {
        let finite = match self {
            Bad::Single(value) => value.is_finite(),
            Bad::Double(value) => value.is_finite(),
        };
        if finite { LIMIT } else { 0.0 }
    }

    /// Gives `meter` a short block of silence with this sample at one
    /// frame of each of the first `hit` channels.
    fn play(self, meter: &mut impl Meter, channels: usize, hit: usize) {
        match self {
            Bad::Single(value) => meter.take_single(&glitch(channels, hit, value)),
            Bad::Double(value) => meter.take_double(&glitch(channels, hit, value)),
        }
    }
}

/// A block of silence with `bad` at one frame of each of the first `hit`
/// channels.
fn glitch<S: Sample>(channels: usize, hit: usize, bad: S) -> AudioBuffer<S> {
    let mut buffer = AudioBuffer::<S>::new(channels, 64);
    for channel in 0..hit {
        buffer.channel_mut(channel)[10] = bad;
    }
    buffer
}

/// One second of a 1 kHz sine at -20 dBFS in every channel. It holds a
/// whole number of cycles, so playing it again and again is one tone.
fn one_second_of_tone(channels: usize) -> AudioBuffer<f64> {
    let mut buffer = AudioBuffer::<f64>::new(channels, 48_000);
    for channel in buffer.iter_channels_mut() {
        for (n, sample) in channel.iter_mut().enumerate() {
            *sample = 0.1 * (TAU * 1_000.0 * n as f64 / RATE).sin();
        }
    }
    buffer
}

/// Plays `seconds` of the tone into `meter`.
fn play_tone(meter: &mut impl Meter, channels: usize, seconds: usize) {
    let tone = one_second_of_tone(channels);
    for _ in 0..seconds {
        meter.take_double(&tone);
    }
}

// ---------------------------------------------------------------------------
// Three cases found by review, as they were reported. Before the meters
// screened their input, each of these readings stayed wrong until a reset:
// infinity survives every decay, and the square of 1e200 or of `f64::MAX`
// is an infinity that turns into NaN one step later.
// ---------------------------------------------------------------------------

#[test]
fn an_infinite_sample_does_not_leave_the_peak_meters_stuck() {
    // A fast fall-back, so that a meter which limited the bad sample instead
    // of dropping it would also be back on the tone within the five seconds.
    let mut peak = PeakMeter::new(1).with_decay(120.0);
    let mut true_peak = TruePeakMeter::new(1).with_decay(120.0);
    peak.prepare(&spec(RATE, 1));
    true_peak.prepare(&spec(RATE, 1));

    // What a filter that blows up upstream produces: one infinite sample.
    let bad = glitch(1, 1, f64::INFINITY);
    peak.measure(&bad);
    true_peak.measure(&bad);
    let good = one_second_of_tone(1);
    for _ in 0..5 {
        peak.measure(&good);
        true_peak.measure(&good);
    }

    // Five seconds of a -20 dBFS tone later, level and marker are back on
    // the tone.
    let mut stuck = Vec::new();
    for (what, reading) in [
        ("PeakMeter level", peak.readings().level_db(0)),
        ("PeakMeter hold marker", peak.readings().held_db(0)),
        ("TruePeakMeter level", true_peak.readings().level_db(0)),
        ("TruePeakMeter hold marker", true_peak.readings().held_db(0)),
    ] {
        println!("{what}: {reading} dB");
        if (reading + 20.0).abs() >= 0.5 {
            stuck.push(format!("{what} reads {reading} dB"));
        }
    }
    assert!(
        stuck.is_empty(),
        "five seconds after one infinite sample, with a tone at -20 dBFS playing: {stuck:?}"
    );
    // An infinite sample is measured as silence, so the maximum since the
    // reset is the tone's as well.
    assert!((peak.readings().max_db(0) + 20.0).abs() < 0.01);
    assert!((true_peak.readings().max_db(0) + 20.0).abs() < 0.01);
}

#[test]
fn a_huge_sample_does_not_leave_the_rms_and_stereo_meters_stuck() {
    // Short time constants, for the same reason as the fast fall-back above.
    let mut rms = RmsMeter::new(2).with_time_constant(0.05);
    let mut stereo = StereoMeter::new().with_time_constant(0.05);
    rms.prepare(&spec(RATE, 2));
    stereo.prepare(&spec(RATE, 2));

    // A finite `f64` whose square is not: about one random bit pattern in
    // four is this large or larger.
    let bad = glitch(2, 1, 1e200);
    rms.measure(&bad);
    stereo.measure(&bad);
    let good = one_second_of_tone(2);
    for _ in 0..5 {
        rms.measure(&good);
        stereo.measure(&good);
    }

    let mut stuck = Vec::new();
    // A sine of amplitude 0.1 has an RMS level of -23.01 dBFS.
    let level = rms.readings().rms_db(0);
    println!("RmsMeter: {level} dB");
    if (level + 23.01).abs() >= 0.1 {
        stuck.push(format!("RmsMeter reads {level} dB, the tone is at -23.01"));
    }

    // The same tone is in both channels: correlation +1, balance 0. And
    // correlation is documented as "from -1 to +1" whatever the input.
    let readings = stereo.readings();
    let (correlation, balance) = (readings.correlation(), readings.balance());
    println!("StereoMeter: correlation {correlation}, balance {balance}");
    if !(0.99..=1.0).contains(&correlation) {
        stuck.push(format!("StereoMeter correlation reads {correlation}"));
    }
    if balance.is_nan() || balance.abs() >= 0.01 {
        stuck.push(format!("StereoMeter balance reads {balance}"));
    }
    assert!(
        stuck.is_empty(),
        "five seconds after one sample of 1e200, with the same tone in both channels: {stuck:?}"
    );
}

#[test]
fn a_huge_sample_does_not_leave_the_vu_meter_stuck() {
    let mut meter = VuMeter::new(1);
    meter.prepare(&spec(RATE, 1));
    // Scaled to the reference level this used to overflow to infinity, and
    // the needle's position became infinity minus infinity.
    meter.measure(&glitch(1, 1, f64::MAX));
    let good = one_second_of_tone(1);
    for _ in 0..5 {
        meter.measure(&good);
    }
    // -20 dBFS against the default reference of -18 dBFS.
    let reading = meter.readings().vu(0);
    println!("VuMeter: {reading} VU");
    assert!(
        (reading + 2.0).abs() < 0.1,
        "VuMeter reads {reading} VU five seconds after one sample of f64::MAX; the tone is at -2"
    );
}

// ---------------------------------------------------------------------------
// Every meter, every kind of bad sample, both precisions.
// ---------------------------------------------------------------------------

#[test]
fn no_bad_sample_leaves_the_peak_meter_stuck() {
    for bad in Bad::all() {
        // The fall-back is fast enough to cover the 200 dB between a
        // sample at the limit and the tone well within the five seconds.
        let mut meter = PeakMeter::new(1).with_decay(120.0);
        meter.prepare(&spec(RATE, 1));
        bad.play(&mut meter, 1, 1);

        // At once: measured as the documentation says, and every reading
        // a number.
        let readings = meter.readings().clone();
        assert_eq!(readings.max(0), bad.measured_as(), "{bad:?}");
        assert_eq!(readings.held(0), bad.measured_as(), "{bad:?}");
        assert!(readings.level(0) <= LIMIT, "{bad:?}: {}", readings.level(0));
        assert!(readings.level(0) >= 0.0, "{bad:?}: {}", readings.level(0));

        play_tone(&mut meter, 1, 5);
        for (what, reading) in [
            ("level", readings.level_db(0)),
            ("hold marker", readings.held_db(0)),
        ] {
            assert!(
                (reading + 20.0).abs() < 0.5,
                "five seconds after {bad:?} the {what} reads {reading} dB"
            );
        }
    }
}

#[test]
fn no_bad_sample_leaves_the_true_peak_meter_stuck() {
    for bad in Bad::all() {
        // Two seconds of tone are enough with this fall-back and hold, and
        // keep the interpolation affordable in an unoptimised build.
        let mut meter = TruePeakMeter::new(1).with_decay(240.0).with_hold(0.5);
        meter.prepare(&spec(RATE, 1));
        bad.play(&mut meter, 1, 1);

        // The bad sample went through the interpolation filter as what it
        // is measured as: nothing larger came out, and nothing that is not
        // a number.
        let readings = meter.readings().clone();
        assert_eq!(readings.max(0), bad.measured_as(), "{bad:?}");
        assert_eq!(readings.held(0), bad.measured_as(), "{bad:?}");
        assert!(readings.level(0) <= LIMIT, "{bad:?}: {}", readings.level(0));
        assert!(readings.level(0) >= 0.0, "{bad:?}: {}", readings.level(0));

        play_tone(&mut meter, 1, 2);
        for (what, reading) in [
            ("level", readings.level_db(0)),
            ("hold marker", readings.held_db(0)),
        ] {
            assert!(
                (reading + 20.0).abs() < 0.5,
                "two seconds after {bad:?} the {what} reads {reading} dBTP"
            );
        }
    }
}

#[test]
fn no_bad_sample_leaves_the_rms_meter_stuck() {
    for bad in Bad::all() {
        // In one channel, and in both.
        for hit in [1, 2] {
            let mut meter = RmsMeter::new(2).with_time_constant(0.05);
            meter.prepare(&spec(RATE, 2));
            bad.play(&mut meter, 2, hit);
            let readings = meter.readings().clone();
            for channel in 0..2 {
                let rms = readings.rms(channel);
                assert!((0.0..=LIMIT).contains(&rms), "{bad:?}: {rms}");
            }

            play_tone(&mut meter, 2, 5);
            for channel in 0..2 {
                // A sine of amplitude 0.1 has an RMS level of -23.01 dBFS.
                let level = readings.rms_db(channel);
                assert!(
                    (level + 23.01).abs() < 0.1,
                    "five seconds after {bad:?}, channel {channel} reads {level} dB"
                );
            }
        }
    }
}

#[test]
fn no_bad_sample_leaves_the_stereo_meter_stuck() {
    for bad in Bad::all() {
        for hit in [1, 2] {
            let mut meter = StereoMeter::new().with_time_constant(0.05);
            meter.prepare(&spec(RATE, 2));
            bad.play(&mut meter, 2, hit);
            let readings = meter.readings().clone();
            // Never outside the range, and never not a number, even with
            // the glitch still in the averages.
            let (correlation, balance) = (readings.correlation(), readings.balance());
            assert!(
                (-1.0..=1.0).contains(&correlation),
                "{bad:?}: {correlation}"
            );
            assert!((-1.0..=1.0).contains(&balance), "{bad:?}: {balance}");
            // At most a channel at the limit against a silent one, which is
            // floored at -100 dBFS.
            let difference = readings.level_difference_db();
            assert!(difference.abs() <= 280.0, "{bad:?}: {difference}");

            play_tone(&mut meter, 2, 5);
            // The same tone in both channels: correlation +1, balance 0.
            let (correlation, balance) = (readings.correlation(), readings.balance());
            assert!(
                (0.99..=1.0).contains(&correlation),
                "five seconds after {bad:?} in {hit} channel(s), correlation reads {correlation}"
            );
            assert!(
                balance.abs() < 0.01,
                "five seconds after {bad:?} in {hit} channel(s), balance reads {balance}"
            );
            assert!(readings.level_difference_db().abs() < 0.1);
        }
    }
}

#[test]
fn no_bad_sample_leaves_the_vu_meter_stuck() {
    for bad in Bad::all() {
        let mut meter = VuMeter::new(1);
        meter.prepare(&spec(RATE, 1));
        bad.play(&mut meter, 1, 1);
        let readings = meter.readings().clone();
        let deflection = readings.deflection(0);
        assert!(deflection.is_finite() && deflection >= 0.0, "{bad:?}");

        play_tone(&mut meter, 1, 5);
        // -20 dBFS against the default reference of -18 dBFS.
        let reading = readings.vu(0);
        assert!(
            (reading + 2.0).abs() < 0.1,
            "five seconds after {bad:?} the needle reads {reading} VU"
        );
    }
}

#[test]
fn no_bad_sample_leaves_the_ppm_stuck() {
    for bad in Bad::all() {
        let mut meter = PpmMeter::new(1);
        meter.prepare(&spec(RATE, 1));
        bad.play(&mut meter, 1, 1);
        let readings = meter.readings().clone();
        let level = readings.level(0);
        assert!((0.0..=LIMIT).contains(&level), "{bad:?}: {level}");

        // The meter's return time is fixed at 20 dB in 1.7 s. A sample at
        // the limit charges it to 144 dB, so the tone 164 dB further down
        // shows through after 14 s.
        play_tone(&mut meter, 1, 15);
        let reading = readings.level_db(0);
        assert!(
            (reading + 20.0).abs() < 0.1,
            "fifteen seconds after {bad:?} the meter reads {reading} dB"
        );
    }
}

#[test]
fn no_bad_sample_leaves_the_loudness_meter_stuck() {
    for bad in Bad::all() {
        for hit in [1, 2] {
            let mut meter = LoudnessMeter::stereo();
            meter.prepare(&spec(RATE, 2));
            bad.play(&mut meter, 2, hit);
            // Minus infinity is a reading (silence); NaN and plus infinity
            // are not.
            let during = meter.readings().snapshot();
            for reading in [during.momentary, during.short_term] {
                assert!(reading < 250.0, "{bad:?}: {reading} LUFS");
            }

            // Four seconds on, the bad sample has left both windows.
            play_tone(&mut meter, 2, 4);
            let after = meter.readings().snapshot();
            for (what, reading) in [
                ("momentary", after.momentary),
                ("short-term", after.short_term),
            ] {
                assert!(
                    (reading - TONE_LUFS).abs() < 0.01,
                    "four seconds after {bad:?}, {what} loudness reads {reading} LUFS"
                );
            }
            // The gated measures add up everything since the last reset,
            // so they keep a sample at the limit until the next one. They
            // are numbers all the same, and a reset clears them.
            assert!(after.integrated.is_finite(), "{bad:?}");
            assert!(after.range.is_finite(), "{bad:?}");
            assert!(after.max_momentary.is_finite(), "{bad:?}");
            meter.reset_integration();
            play_tone(&mut meter, 2, 4);
            let fresh = meter.readings().snapshot();
            for reading in [fresh.integrated, fresh.max_momentary, fresh.max_short_term] {
                assert!((reading - TONE_LUFS).abs() < 0.01, "{bad:?}: {reading}");
            }
            assert!(fresh.range < 0.02, "{bad:?}: {}", fresh.range);
        }
    }
}

#[test]
fn no_bad_sample_leaves_the_spectrum_analyzer_stuck() {
    for bad in Bad::all() {
        // Bins 11.7 Hz apart: the tone's 1 kHz lies a third of the way
        // from bin 85 to bin 86, and the flat-top window reads it at its
        // level all the same. A peak hold short enough to run out and fall
        // back within the five seconds.
        let settings = SpectrumSettings::new(4_096)
            .with_hop(4_096)
            .with_window(Window::FlatTop)
            .with_bands(OctaveFraction::Third)
            .with_peak_hold(0.1, 100.0);
        let (mut analyzer, mut reader) = SpectrumAnalyzer::new(settings);
        analyzer.prepare(&spec(RATE, 2));
        bad.play(&mut analyzer, 2, 1);
        play_tone(&mut analyzer, 2, 1);
        // Every reading of every frame since is a number.
        let frame = reader.latest();
        for values in [
            frame.magnitudes_db(),
            frame.peaks_db(),
            frame.bands_db(),
            frame.band_peaks_db(),
        ] {
            assert!(values.iter().all(|value| value.is_finite()), "{bad:?}");
        }
        play_tone(&mut analyzer, 2, 4);
        // The tone at -20 dBFS, in its bin and its band, and the peaks
        // have come back to it.
        let band = analyzer
            .bands()
            .iter()
            .position(|band| band.nominal() == 1_000.0)
            .unwrap();
        for (what, reading) in [
            ("bin", analyzer.magnitudes_db()[85]),
            ("peak", analyzer.peaks_db()[85]),
            ("band", analyzer.bands_db()[band]),
            ("band peak", analyzer.band_peaks_db()[band]),
        ] {
            assert!(
                (reading + 20.0).abs() < 0.02,
                "five seconds after {bad:?}, the {what} reads {reading} dB"
            );
        }
        // Far from the tone, the click has gone from the peaks as well.
        assert!(
            analyzer.peaks_db()[1_000] < -100.0,
            "{bad:?}: {}",
            analyzer.peaks_db()[1_000]
        );
    }
}

#[test]
fn no_bad_sample_leaves_the_pitch_detector_stuck() {
    for method in [PitchMethod::yin(), PitchMethod::mpm()] {
        for bad in Bad::all() {
            // Only the last analysis matters here: a long hop keeps the
            // test quick.
            let mut detector = PitchDetector::new(PitchSettings::new(method).with_hop(4_096));
            detector.prepare(&spec(RATE, 2));
            bad.play(&mut detector, 2, 2);
            play_tone(&mut detector, 2, 1);
            let pitch = detector.readings().pitch();
            assert!(
                (pitch.frequency - 1_000.0).abs() < 0.1 && pitch.confidence > 0.99,
                "{method:?}, a second after {bad:?}: {pitch:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// A sample at the limit is still a very loud sample. How long each reading
// takes to come back from it is a matter of the meter's ballistics, and the
// documentation of each meter says how long that is at 48 kHz with the
// default settings. These are those figures.
// ---------------------------------------------------------------------------

/// Plays one sample of `f64::MAX` and then `seconds` of silence in blocks
/// of 10 ms. Returns the highest reading, in decibels, and the time in
/// seconds of the last block that ended with the reading at or above 0 dB.
fn fade<M: Meter>(meter: &mut M, seconds: usize, reading_db: impl Fn(&M) -> f64) -> (f64, f64) {
    let mut click = AudioBuffer::<f64>::new(1, 1);
    click.channel_mut(0)[0] = f64::MAX;
    meter.take_double(&click);
    let mut highest = reading_db(meter);
    let mut last_over = 0.0;
    let silence = AudioBuffer::<f64>::new(1, 480);
    for block in 1..=seconds * 100 {
        meter.take_double(&silence);
        let reading = reading_db(meter);
        highest = highest.max(reading);
        if reading >= 0.0 {
            last_over = block as f64 / 100.0;
        }
    }
    (highest, last_over)
}

/// Checks one measured fade against the figures the documentation quotes.
#[track_caller]
fn check_fade(what: &str, measured: (f64, f64), highest: f64, back_after: f64) {
    println!(
        "{what}: one sample at the limit reads {:.2} dB and is back under 0 dB after {:.2} s",
        measured.0, measured.1
    );
    assert!(
        (measured.0 - highest).abs() < 0.5,
        "{what} reads {:.2} dB, the documentation says {highest}",
        measured.0
    );
    assert!(
        (measured.1 - back_after).abs() < 0.05,
        "{what} is back after {:.2} s, the documentation says {back_after}",
        measured.1
    );
}

#[test]
fn a_sample_at_the_limit_fades_at_each_meters_own_pace() {
    // 180 dB at 20 dB in 1.7 s.
    let mut peak = PeakMeter::new(1);
    peak.prepare(&spec(RATE, 1));
    let measured = fade(&mut peak, 20, |meter| meter.readings().level_db(0));
    check_fade("PeakMeter", measured, 180.0, 15.3);
    // The marker stayed on it for its two seconds, and the maximum stays.
    assert_eq!(peak.readings().held(0), peak.readings().level(0));
    assert_eq!(peak.readings().max(0), LIMIT);

    let mut true_peak = TruePeakMeter::new(1);
    true_peak.prepare(&spec(RATE, 1));
    let measured = fade(&mut true_peak, 20, |meter| meter.readings().level_db(0));
    check_fade("TruePeakMeter", measured, 180.0, 15.3);
    assert_eq!(true_peak.readings().max(0), LIMIT);

    let mut rms = RmsMeter::new(1);
    rms.prepare(&spec(RATE, 1));
    let measured = fade(&mut rms, 20, |meter| meter.readings().rms_db(0));
    check_fade("RmsMeter", measured, 138.4, 9.56);

    let mut ppm = PpmMeter::new(1);
    ppm.prepare(&spec(RATE, 1));
    let measured = fade(&mut ppm, 20, |meter| meter.readings().level_db(0));
    check_fade("PpmMeter", measured, 143.8, 12.22);

    let mut vu = VuMeter::new(1);
    vu.prepare(&spec(RATE, 1));
    let measured = fade(&mut vu, 20, |meter| meter.readings().vu(0));
    // The needle swings through rest and back twice on its way down. The
    // last time it is above 0 VU is 1.18 s after the sample.
    check_fade("VuMeter", measured, 123.4, 1.18);
}

#[test]
fn a_sample_at_the_limit_leaves_the_windows_and_the_stereo_averages_in_time() {
    // The tone keeps playing, in blocks of 10 ms, and the bad sample is in
    // the left channel only.
    let tone = one_second_of_tone(2);
    let mut block = AudioBuffer::<f64>::new(2, 480);
    let mut loudness = LoudnessMeter::stereo();
    let mut stereo = StereoMeter::new();
    loudness.prepare(&spec(RATE, 2));
    stereo.prepare(&spec(RATE, 2));
    loudness.measure(&glitch(2, 1, f64::MAX));
    stereo.measure(&glitch(2, 1, f64::MAX));
    // With the glitch in its average the left channel is all there is.
    assert_eq!(stereo.readings().balance(), -1.0);
    assert_eq!(stereo.readings().correlation(), 0.0);

    let (mut momentary_off, mut short_term_off, mut correlation_off, mut balance_off) =
        (0.0, 0.0, 0.0, 0.0);
    for index in 0..1_500 {
        let start = (index % 100) * 480;
        for (target, source) in block.iter_channels_mut().zip(tone.iter_channels()) {
            target.copy_from_slice(&source[start..start + 480]);
        }
        loudness.measure(&block);
        stereo.measure(&block);
        let time = (index + 1) as f64 / 100.0;
        if (loudness.readings().momentary() - TONE_LUFS).abs() >= 0.1 {
            momentary_off = time;
        }
        if (loudness.readings().short_term() - TONE_LUFS).abs() >= 0.1 {
            short_term_off = time;
        }
        if stereo.readings().correlation() < 0.99 {
            correlation_off = time;
        }
        if stereo.readings().balance().abs() > 0.01 {
            balance_off = time;
        }
    }
    println!(
        "after one sample at the limit: momentary loudness is off the tone for {momentary_off} s, \
         short-term for {short_term_off} s, correlation for {correlation_off} s, balance for \
         {balance_off} s"
    );
    // The windows are 400 ms and 3 s long, and the high-pass of the
    // K-weighting rings for some 70 ms after a sample that large: half a
    // second and 3.1 s, as the documentation of `LoudnessMeter` has it.
    assert!((momentary_off - 0.48).abs() < 0.015, "{momentary_off}");
    assert!((short_term_off - 3.07).abs() < 0.015, "{short_term_off}");
    // The stereo meter's averages fade by a factor of e every 300 ms.
    assert!((correlation_off - 12.3).abs() < 0.05, "{correlation_off}");
    assert!((balance_off - 12.1).abs() < 0.05, "{balance_off}");
}
