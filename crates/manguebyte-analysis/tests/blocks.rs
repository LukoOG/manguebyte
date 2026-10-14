//! What must not change a reading: how the audio is cut into blocks, which
//! sample rate it has, and who is reading.

mod common;

use core::f64::consts::TAU;

use common::{Noise, in_blocks, spec};
use manguebyte_analysis::{
    LagInterpolation, LoudnessMeter, OctaveFraction, PeakMeter, PitchDetector, PitchMethod,
    PitchSettings, PpmMeter, RmsMeter, Source, SpectrumAnalyzer, SpectrumSettings, StereoMeter,
    TruePeakMeter, VuMeter, Weighting,
};
use manguebyte_core::AudioBuffer;
use manguebyte_dsp::window::Window;

/// Five seconds of stereo programme at 48 kHz: tones that come and go,
/// noise, a few silences and some clicks, different in the two channels.
fn programme() -> AudioBuffer<f32> {
    let frames = 5 * 48_000;
    let mut noise = Noise(42);
    let mut buffer = AudioBuffer::<f32>::new(2, frames);
    for index in 0..2 {
        let channel = buffer.channel_mut(index);
        for (n, sample) in channel.iter_mut().enumerate() {
            let time = n as f64 / 48_000.0;
            // A slow swell, and half a second of silence in the fourth.
            let swell = 0.5 + 0.45 * (TAU * 0.4 * time + index as f64).sin();
            let gate = if (3.0..3.5).contains(&time) { 0.0 } else { 1.0 };
            let tones = 0.3 * (TAU * 220.0 * time).sin()
                + 0.2 * (TAU * (3_100.0 + 400.0 * index as f64) * time).sin();
            let click = if n % 7_919 == 0 { 0.9 } else { 0.0 };
            *sample = (gate * swell * (tones + 0.1 * noise.next()) + click) as f32;
        }
    }
    buffer
}

/// The block patterns every meter is run with. The first is the reference.
const PATTERNS: [&[usize]; 7] = [
    &[480],
    &[1],
    &[64],
    &[997],
    &[1, 511, 7, 300, 64, 2, 129],
    &[4_800],
    &[240_000],
];

/// Runs `readings_for` with every block pattern and requires the same bits
/// from each.
fn assert_same_for_every_pattern(what: &str, readings_for: impl Fn(&[usize]) -> Vec<u64>) {
    let reference = readings_for(PATTERNS[0]);
    assert!(
        reference.iter().any(|&bits| f64::from_bits(bits) != 0.0),
        "{what} read nothing at all"
    );
    for pattern in &PATTERNS[1..] {
        assert_eq!(
            readings_for(pattern),
            reference,
            "{what} depends on the block size: differs for {pattern:?}"
        );
    }
}

#[test]
fn loudness_does_not_depend_on_the_block_size() {
    let signal = programme();
    assert_same_for_every_pattern("LoudnessMeter", |pattern| {
        let mut meter = LoudnessMeter::stereo();
        meter.prepare(&spec(48_000.0, 2));
        in_blocks(&signal, pattern, |block| meter.measure(block));
        let loudness = meter.readings().snapshot();
        // The programme must have exercised every reading.
        assert!(loudness.integrated.is_finite() && loudness.range > 0.5);
        assert!(loudness.max_momentary > loudness.integrated);
        [
            loudness.momentary,
            loudness.short_term,
            loudness.integrated,
            loudness.range,
            loudness.max_momentary,
            loudness.max_short_term,
            meter.readings().measured_seconds(),
        ]
        .iter()
        .map(|value| value.to_bits())
        .collect()
    });
}

#[test]
fn true_peak_does_not_depend_on_the_block_size() {
    // A quarter of a second is enough here, with a hold short enough to
    // run out after each of its two clicks, and keeps the 16-times filter
    // affordable in an unoptimised build.
    let mut signal = programme();
    signal.set_num_frames(12_000);
    for factor in [2, 4, 8, 16] {
        assert_same_for_every_pattern("TruePeakMeter", |pattern| {
            let mut meter = TruePeakMeter::new(2)
                .with_oversampling(factor)
                .with_hold(0.05);
            meter.prepare(&spec(48_000.0, 2));
            in_blocks(&signal, pattern, |block| meter.measure(block));
            let read = |meter: &TruePeakMeter| -> Vec<u64> {
                let readings = meter.readings();
                (0..2)
                    .flat_map(|channel| {
                        [
                            readings.level(channel),
                            readings.held(channel),
                            readings.max(channel),
                        ]
                    })
                    .map(f64::to_bits)
                    .collect()
            };
            // As the last block left them, and again once the end of the
            // signal has been flushed out of the filter.
            let mut bits = read(&meter);
            meter.flush();
            bits.extend(read(&meter));
            bits
        });
    }
}

#[test]
fn level_meters_do_not_depend_on_the_block_size() {
    let signal = programme();
    assert_same_for_every_pattern("PeakMeter", |pattern| {
        let mut meter = PeakMeter::new(2).with_hold(0.3);
        meter.prepare(&spec(48_000.0, 2));
        in_blocks(&signal, pattern, |block| meter.measure(block));
        let readings = meter.readings();
        (0..2)
            .flat_map(|channel| {
                [
                    readings.level(channel),
                    readings.held(channel),
                    readings.max(channel),
                ]
            })
            .map(f64::to_bits)
            .collect()
    });
    assert_same_for_every_pattern("RmsMeter", |pattern| {
        let mut meter = RmsMeter::new(2);
        meter.prepare(&spec(48_000.0, 2));
        in_blocks(&signal, pattern, |block| meter.measure(block));
        let readings = meter.readings();
        vec![readings.rms(0).to_bits(), readings.rms(1).to_bits()]
    });
    assert_same_for_every_pattern("VuMeter", |pattern| {
        let mut meter = VuMeter::new(2);
        meter.prepare(&spec(48_000.0, 2));
        in_blocks(&signal, pattern, |block| meter.measure(block));
        let readings = meter.readings();
        vec![
            readings.deflection(0).to_bits(),
            readings.deflection(1).to_bits(),
        ]
    });
    assert_same_for_every_pattern("PpmMeter", |pattern| {
        let mut meter = PpmMeter::new(2);
        meter.prepare(&spec(48_000.0, 2));
        in_blocks(&signal, pattern, |block| meter.measure(block));
        let readings = meter.readings();
        vec![readings.level(0).to_bits(), readings.level(1).to_bits()]
    });
    assert_same_for_every_pattern("StereoMeter", |pattern| {
        let mut meter = StereoMeter::new();
        meter.prepare(&spec(48_000.0, 2));
        in_blocks(&signal, pattern, |block| meter.measure(block));
        let readings = meter.readings();
        vec![
            readings.correlation().to_bits(),
            readings.balance().to_bits(),
            readings.level_difference_db().to_bits(),
        ]
    });
}

#[test]
fn the_spectrum_does_not_depend_on_the_block_size() {
    // A transform size FFT does by Bluestein's method, a hop that does not
    // divide it, and everything that keeps state between frames switched
    // on: ballistics, a peak hold that runs out, and bands. Two seconds of
    // the programme keep the unoptimised build quick.
    let mut signal = programme();
    signal.set_num_frames(96_000);
    for source in [Source::Mean, Source::Channel(1)] {
        assert_same_for_every_pattern("SpectrumAnalyzer", |pattern| {
            let settings = SpectrumSettings::new(1_000)
                .with_hop(333)
                .with_window(Window::BlackmanHarris)
                .with_weighting(Weighting::A)
                .with_bands(OctaveFraction::Third)
                .with_ballistics(0.02, 0.3)
                .with_peak_hold(0.2, 30.0)
                .with_source(source);
            let (mut analyzer, mut reader) = SpectrumAnalyzer::new(settings);
            analyzer.prepare(&spec(48_000.0, 2));
            in_blocks(&signal, pattern, |block| analyzer.measure(block));
            let frame = reader.latest();
            assert_eq!(frame.frame_count(), analyzer.frame_count());
            let mut bits: Vec<u64> = [
                analyzer.magnitudes_db(),
                analyzer.peaks_db(),
                analyzer.bands_db(),
                analyzer.band_peaks_db(),
            ]
            .iter()
            .flat_map(|values| values.iter().map(|value| value.to_bits()))
            .collect();
            bits.extend(
                frame
                    .magnitudes_db()
                    .iter()
                    .chain(frame.bands_db())
                    .map(|value| u64::from(value.to_bits())),
            );
            bits.push(analyzer.frame_count());
            bits
        });
    }
}

/// Feeds `signal` to `sink` cut into blocks whose lengths cycle through
/// `pattern`, never across a multiple of `every` frames, and calls
/// `checkpoint` at each of those multiples.
fn in_blocks_with_checkpoints(
    signal: &AudioBuffer<f32>,
    pattern: &[usize],
    every: usize,
    mut sink: impl FnMut(&AudioBuffer<f32>),
    mut checkpoint: impl FnMut(),
) {
    let largest = pattern.iter().copied().max().unwrap_or(1).max(1);
    let mut block = AudioBuffer::<f32>::new(signal.num_channels(), largest);
    let (mut start, mut turn) = (0, 0);
    while start < signal.num_frames() {
        let next_checkpoint = (start / every + 1) * every;
        let frames = pattern[turn % pattern.len()]
            .max(1)
            .min(signal.num_frames().min(next_checkpoint) - start);
        turn += 1;
        block.set_num_frames(frames);
        for (channel, source) in block.iter_channels_mut().zip(signal.iter_channels()) {
            channel.copy_from_slice(&source[start..start + frames]);
        }
        sink(&block);
        start += frames;
        if start % every == 0 {
            checkpoint();
        }
    }
}

#[test]
fn the_pitch_does_not_depend_on_the_block_size() {
    // Every analysis is a function of the samples alone; the estimate is
    // compared every 0.1 s, so a difference anywhere would show. Both
    // methods and both interpolations are covered, two at a time, to keep
    // the unoptimised build quick.
    let mut signal = programme();
    signal.set_num_frames(48_000);
    for (method, interpolation) in [
        (PitchMethod::yin(), LagInterpolation::BandLimited),
        (PitchMethod::mpm(), LagInterpolation::Parabolic),
    ] {
        assert_same_for_every_pattern("PitchDetector", |pattern| {
            let settings = PitchSettings::new(method)
                .with_range(80.0, 1_500.0)
                .with_hop(397)
                .with_interpolation(interpolation);
            let mut detector = PitchDetector::new(settings);
            detector.prepare(&spec(48_000.0, 2));
            let readings = detector.readings().clone();
            let mut bits = Vec::new();
            in_blocks_with_checkpoints(
                &signal,
                pattern,
                4_800,
                |block| detector.measure(block),
                || {
                    let pitch = readings.pitch();
                    bits.push(pitch.frequency.to_bits());
                    bits.push(pitch.confidence.to_bits());
                    bits.push(readings.analysis_count());
                },
            );
            let last = detector.latest();
            bits.extend([last.frequency.to_bits(), last.confidence.to_bits()]);
            bits
        });
    }
}

/// A programme that can be written down for any sample rate: six tones
/// from 60 Hz to 12 kHz, different in the two channels, 10 s at one level
/// and 10 s at a level 12 dB lower.
fn loudness_at(sample_rate: f64) -> [f64; 5] {
    const TONES: [(f64, f64, f64); 6] = [
        (60.0, 0.10, 0.08),
        (250.0, 0.08, 0.10),
        (1_000.0, 0.05, 0.05),
        (2_500.0, 0.05, 0.03),
        (6_000.0, 0.03, 0.04),
        (12_000.0, 0.02, 0.02),
    ];
    let mut meter = LoudnessMeter::stereo();
    meter.prepare(&spec(sample_rate, 2));
    let frames = (sample_rate / 10.0).round() as usize;
    let mut block = AudioBuffer::<f64>::new(2, frames);
    for index in 0..200 {
        let level = if index < 100 { 1.0 } else { 0.25 };
        block.clear();
        let (left_channel, right_channel) = block.channel_pair_mut(0, 1);
        for (frequency, left, right) in TONES {
            for (frame, (l, r)) in left_channel
                .iter_mut()
                .zip(right_channel.iter_mut())
                .enumerate()
            {
                let time = (index * frames + frame) as f64 / sample_rate;
                let sine = level * (TAU * frequency * time).sin();
                *l += left * sine;
                *r += right * sine;
            }
        }
        meter.measure(&block);
    }
    let loudness = meter.readings().snapshot();
    [
        loudness.integrated,
        loudness.max_momentary,
        loudness.max_short_term,
        loudness.short_term,
        loudness.range,
    ]
}

#[test]
fn the_same_signal_reads_the_same_at_every_sample_rate() {
    let reference = loudness_at(48_000.0);
    println!("at 48000 Hz: I, max M, max S, S, LRA = {reference:.4?}");
    // The programme is what it was meant to be: two levels 12 dB apart.
    assert!(reference[0] > -35.0 && reference[0] < -10.0);
    assert!((reference[4] - 12.04).abs() < 0.05, "{}", reference[4]);

    let mut worst = 0.0f64;
    for sample_rate in [44_100.0, 88_200.0, 96_000.0, 192_000.0] {
        let readings = loudness_at(sample_rate);
        println!("at {sample_rate} Hz: I, max M, max S, S, LRA = {readings:.4?}");
        for (reading, expected) in readings.iter().zip(reference) {
            worst = worst.max((reading - expected).abs());
            assert!(
                (reading - expected).abs() < 0.05,
                "{sample_rate} Hz: {reading} against {expected} at 48 kHz"
            );
        }
    }
    println!("largest difference from 48 kHz: {worst:.4} LU");
}

#[test]
fn the_tone_of_case_1_reads_the_same_at_every_sample_rate() {
    // Tech 3341 case 1 again, 20 s of 1 kHz at -23 dBFS in both channels,
    // at the rates the EBU does not publish files for.
    for sample_rate in [44_100.0, 48_000.0, 88_200.0, 96_000.0, 192_000.0] {
        let mut meter = LoudnessMeter::stereo();
        meter.prepare(&spec(sample_rate, 2));
        common::Tone::new(sample_rate, 1_000.0, 2).play(&common::both(-23.0), 20.0, |block| {
            meter.measure(block);
        });
        let loudness = meter.readings().snapshot();
        println!(
            "1 kHz at -23 dBFS, {sample_rate} Hz: M {:.4}, S {:.4}, I {:.4}",
            loudness.momentary, loudness.short_term, loudness.integrated
        );
        for reading in [loudness.momentary, loudness.short_term, loudness.integrated] {
            // Within 0.05 LU of each other means within 0.025 of the
            // middle; all of them are closer than that to -22.99.
            assert!((reading + 22.99).abs() < 0.025, "{sample_rate}: {reading}");
        }
    }
}

#[test]
fn single_and_double_precision_blocks_read_the_same() {
    let single = programme();
    let mut double = AudioBuffer::<f64>::new(2, single.num_frames());
    for (target, source) in double.iter_channels_mut().zip(single.iter_channels()) {
        for (sample, &value) in target.iter_mut().zip(source) {
            *sample = f64::from(value);
        }
    }
    let (mut loudness_a, mut loudness_b) = (LoudnessMeter::stereo(), LoudnessMeter::stereo());
    let (mut peak_a, mut peak_b) = (TruePeakMeter::new(2), TruePeakMeter::new(2));
    for meter in [&mut loudness_a, &mut loudness_b] {
        meter.prepare(&spec(48_000.0, 2));
    }
    for meter in [&mut peak_a, &mut peak_b] {
        meter.prepare(&spec(48_000.0, 2));
    }
    loudness_a.measure(&single);
    loudness_b.measure(&double);
    peak_a.measure(&single);
    peak_b.measure(&double);
    // The meters work in double precision whatever they are given, so the
    // same values give the same bits.
    assert_eq!(
        loudness_a.readings().snapshot(),
        loudness_b.readings().snapshot()
    );
    assert_eq!(peak_a.readings().max(0), peak_b.readings().max(0));
    assert_eq!(peak_a.readings().max(1), peak_b.readings().max(1));
}

#[test]
fn readings_can_be_taken_from_another_thread_while_the_meter_runs() {
    let mut loudness = LoudnessMeter::stereo();
    let mut true_peak = TruePeakMeter::new(2);
    loudness.prepare(&spec(48_000.0, 2));
    true_peak.prepare(&spec(48_000.0, 2));
    let (loudness_readings, peak_readings) =
        (loudness.readings().clone(), true_peak.readings().clone());
    // Two seconds of it are plenty for a reader to get its turns in.
    let mut signal = programme();
    signal.set_num_frames(96_000);

    let reader = std::thread::spawn(move || {
        // Poll like a user interface until the audio side says it is done
        // by pausing the meter; every value seen must make sense.
        let mut polls = 0u64;
        let mut highest = f64::NEG_INFINITY;
        // The time limit only matters if the audio side fails first.
        let started = std::time::Instant::now();
        while !loudness_readings.is_paused() && started.elapsed().as_secs() < 300 {
            let snapshot = loudness_readings.snapshot();
            for reading in [snapshot.momentary, snapshot.short_term, snapshot.integrated] {
                assert!(reading == f64::NEG_INFINITY || (-120.0..20.0).contains(&reading));
            }
            assert!((0.0..100.0).contains(&snapshot.range));
            let peak = peak_readings.max_overall();
            assert!((0.0..4.0).contains(&peak));
            assert!(peak >= highest, "the maximum went down");
            highest = highest.max(peak);
            polls += 1;
            std::thread::yield_now();
        }
        polls
    });

    in_blocks(&signal, &[256], |block| {
        loudness.measure(block);
        true_peak.measure(block);
    });
    loudness.readings().set_paused(true);
    let polls = reader.join().unwrap();
    assert!(polls > 0);
    assert!(loudness.readings().integrated().is_finite());
}

#[test]
fn meters_and_readings_may_cross_threads() {
    use manguebyte_analysis::{
        LoudnessReadings, PeakReadings, PpmReadings, RmsReadings, StereoReadings, TruePeakReadings,
        VuReadings,
    };
    // A meter is built on one thread and moved to the audio thread; its
    // readings are shared with any number of others. Checked by the
    // compiler: this test has nothing to do at run time.
    fn moves<T: Send + 'static>() {}
    fn shares<T: Send + Sync + Clone + 'static>() {}
    moves::<LoudnessMeter>();
    moves::<TruePeakMeter>();
    moves::<PeakMeter>();
    moves::<RmsMeter>();
    moves::<VuMeter>();
    moves::<PpmMeter>();
    moves::<StereoMeter>();
    moves::<SpectrumAnalyzer>();
    moves::<manguebyte_analysis::SpectrumReader>();
    moves::<PitchDetector>();
    shares::<manguebyte_analysis::PitchReadings>();
    shares::<LoudnessReadings>();
    shares::<TruePeakReadings>();
    shares::<PeakReadings>();
    shares::<RmsReadings>();
    shares::<VuReadings>();
    shares::<PpmReadings>();
    shares::<StereoReadings>();
}
