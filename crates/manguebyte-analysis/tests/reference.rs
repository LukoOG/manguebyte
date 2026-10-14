//! The loudness meter against the definitions, computed the slow way.
//!
//! The meter never holds more than three seconds of history and a
//! histogram. This test measures the same programme by the book instead:
//! the whole K-weighted signal in memory, every 400 ms gating block and
//! every 3 s window cut out of it and kept in a list, and BS.1770-4
//! equations (3) to (7) and the MATLAB function of EBU Tech 3342 applied to
//! the lists. The two must agree.
//!
//! A second test builds the one kind of programme on which the loudness
//! range does not agree with that function, and checks what the meter
//! promises for it instead.
//!
//! Run with `--nocapture` to see the readings.

mod common;

use core::f64::consts::TAU;

use common::{Noise, spec};
use manguebyte_analysis::{ChannelWeight, KWeighting, LoudnessMeter};
use manguebyte_core::AudioBuffer;
use manguebyte_dsp::{Biquad, Processor};

const RATE: f64 = 48_000.0;
const SECONDS: usize = 14;
const FRAMES: usize = SECONDS * 48_000;

/// A 5.1 programme with a different mixture of tone and noise in every
/// channel, and passages at very different levels: some above the relative
/// gate, one below it, one of silence.
fn programme() -> AudioBuffer<f64> {
    // Level in decibels over each second.
    const LEVELS: [f64; SECONDS] = [
        -20.0, -20.0, -22.0, -45.0, -47.0, -14.0, -14.0, -12.0, -16.0, -200.0, -26.0, -30.0, -18.0,
        -24.0,
    ];
    let mut noise = Noise(2024);
    let mut buffer = AudioBuffer::<f64>::new(6, FRAMES);
    for index in 0..6 {
        let frequency = 90.0 * (index + 1) as f64 * (index + 2) as f64;
        for (n, sample) in buffer.channel_mut(index).iter_mut().enumerate() {
            let time = n as f64 / RATE;
            let level = 10f64.powf(LEVELS[n / 48_000] / 20.0);
            *sample = level * (0.6 * (TAU * frequency * time).sin() + 0.5 * noise.next());
        }
    }
    buffer
}

fn lufs(mean_square: f64) -> f64 {
    -0.691 + 10.0 * mean_square.log10()
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

#[test]
fn the_meter_agrees_with_the_definitions_computed_from_the_whole_programme() {
    let signal = programme();
    let weights = [
        ChannelWeight::Unity,
        ChannelWeight::Unity,
        ChannelWeight::Unity,
        ChannelWeight::Excluded,
        ChannelWeight::Surround,
        ChannelWeight::Surround,
    ];

    // The meter, fed in awkward blocks.
    let mut meter = LoudnessMeter::surround_5_1();
    meter.prepare(&spec(RATE, 6));
    let mut block = AudioBuffer::<f64>::new(6, 1_000);
    let mut start = 0;
    while start < FRAMES {
        let frames = 1_000.min(FRAMES - start);
        block.set_num_frames(frames);
        for (target, source) in block.iter_channels_mut().zip(signal.iter_channels()) {
            target.copy_from_slice(&source[start..start + frames]);
        }
        meter.measure(&block);
        start += frames;
    }
    let measured = meter.readings().snapshot();

    // By the book. The K-weighting is run with the framework's own biquad,
    // not the meter's filter code.
    let filters = KWeighting::for_sample_rate(RATE);
    let mut weighted = signal.clone();
    for coefficients in [filters.shelf, filters.highpass] {
        let mut filter = Biquad::<f64>::new(coefficients);
        filter.prepare(&spec(RATE, 6));
        filter.process(&mut weighted);
    }
    // Equation (2) without the logarithm, for every sample: the sum over
    // the channels of weight times squared sample.
    let mut power = vec![0.0f64; FRAMES];
    for (channel, weight) in weighted.iter_channels().zip(weights) {
        for (sum, &sample) in power.iter_mut().zip(channel) {
            *sum += weight.gain() * sample * sample;
        }
    }
    // Every window of `length` samples that ends on a multiple of `hop`
    // and lies wholly inside the programme, as a mean square.
    let windows = |length: usize, hop: usize| -> Vec<f64> {
        (0..)
            .map(|index| index * hop)
            .take_while(|&start| start + length <= FRAMES)
            .map(|start| mean(&power[start..start + length]))
            .collect()
    };

    // Integrated loudness: 400 ms blocks, 75% overlap, two gates.
    let blocks = windows(19_200, 4_800);
    let above_absolute: Vec<f64> = blocks
        .iter()
        .copied()
        .filter(|&block| lufs(block) > -70.0)
        .collect();
    let relative_gate = lufs(mean(&above_absolute)) - 10.0;
    let gated: Vec<f64> = above_absolute
        .iter()
        .copied()
        .filter(|&block| lufs(block) > relative_gate)
        .collect();
    let integrated = lufs(mean(&gated));
    // The programme must really exercise both gates.
    assert!(above_absolute.len() < blocks.len());
    assert!(gated.len() < above_absolute.len());
    println!(
        "integrated: {integrated:.6} LUFS by the book, {:.6} LUFS measured; {} of {} blocks pass",
        measured.integrated,
        gated.len(),
        blocks.len()
    );
    assert!((measured.integrated - integrated).abs() < 1e-3);

    // Loudness range: 3 s windows at 10 Hz, through the function printed in
    // Tech 3342.
    let short_term: Vec<f64> = windows(144_000, 4_800).into_iter().map(lufs).collect();
    let above: Vec<f64> = short_term
        .iter()
        .copied()
        .filter(|&loudness| loudness >= -70.0)
        .collect();
    let power_mean = mean(
        &above
            .iter()
            .map(|&loudness| 10f64.powf(loudness / 10.0))
            .collect::<Vec<f64>>(),
    );
    let gate = 10.0 * power_mean.log10() - 20.0;
    let mut kept: Vec<f64> = above
        .into_iter()
        .filter(|&loudness| loudness >= gate)
        .collect();
    kept.sort_by(f64::total_cmp);
    let n = kept.len() as f64;
    let low = kept[((n - 1.0) * 10.0 / 100.0 + 1.0).round() as usize - 1];
    let high = kept[((n - 1.0) * 95.0 / 100.0 + 1.0).round() as usize - 1];
    let range = high - low;
    println!(
        "range: {range:.4} LU by the book, {:.4} LU measured",
        measured.range
    );
    assert!(range > 3.0, "the programme is too even: {range}");
    assert!((measured.range - range).abs() <= 0.0101);

    // The windows themselves, at the end and at their maxima. The meter
    // slides them in steps of 10 ms.
    let momentary = windows(19_200, 480);
    let short_term = windows(144_000, 480);
    let last = |values: &[f64]| lufs(values[values.len() - 1]);
    let highest = |values: &[f64]| lufs(values.iter().copied().fold(0.0, f64::max));
    for (what, ours, theirs) in [
        ("momentary", measured.momentary, last(&momentary)),
        ("short-term", measured.short_term, last(&short_term)),
        ("max momentary", measured.max_momentary, highest(&momentary)),
        (
            "max short-term",
            measured.max_short_term,
            highest(&short_term),
        ),
    ] {
        println!("{what}: {theirs:.9} LUFS by the book, {ours:.9} LUFS measured");
        assert!((ours - theirs).abs() < 1e-9, "{what}: {ours} vs {theirs}");
    }
}

// ---------------------------------------------------------------------------
// Where the loudness range stops agreeing with the definition, and how far.
//
// The meter keeps its short-term values in a histogram of 0.01 LU bins, and
// its relative gate keeps or drops the values of a bin together. When values
// lie in the gate's own bin on both sides of the gate, the function of Tech
// 3342 keeps some of them and the meter keeps all or none. What the meter
// promises then is the range that function gives for a gate less than
// 0.01 LU from the true one. That can be a very different number, because
// the function is discontinuous in its gate, and this test builds the case.
// ---------------------------------------------------------------------------

/// The relative gate of the function printed in EBU Tech 3342 for a list of
/// short-term loudness values in LUFS.
fn tech_3342_gate(short_term: &[f64]) -> f64 {
    let above: Vec<f64> = short_term
        .iter()
        .copied()
        .filter(|&loudness| loudness >= -70.0)
        .collect();
    let power = above
        .iter()
        .map(|&loudness| 10f64.powf(loudness / 10.0))
        .sum::<f64>()
        / above.len() as f64;
    10.0 * power.log10() - 20.0
}

/// The rest of that function: the range for a relative gate at `gate` LUFS.
fn tech_3342_range(short_term: &[f64], gate: f64) -> f64 {
    let mut kept: Vec<f64> = short_term
        .iter()
        .copied()
        .filter(|&loudness| loudness >= -70.0 && loudness >= gate)
        .collect();
    kept.sort_by(f64::total_cmp);
    let n = kept.len() as f64;
    let low = kept[((n - 1.0) * 10.0 / 100.0 + 1.0).round() as usize - 1];
    let high = kept[((n - 1.0) * 95.0 / 100.0 + 1.0).round() as usize - 1];
    high - low
}

/// Plays 13 s of a stereo 1 kHz tone whose level creeps from `quiet_from`
/// to `quiet_to` dBFS and then 33 s at `loud` dBFS. Returns the meter's
/// loudness range and the short-term values it was computed from: the
/// short-term reading every 100 ms from three seconds in.
fn creeping_tone_then_loud(quiet_from: f64, quiet_to: f64, loud: f64) -> (f64, Vec<f64>) {
    const QUIET_FRAMES: f64 = 13.0 * RATE;
    const BLOCKS: usize = (13 + 33) * 100;
    let mut meter = LoudnessMeter::stereo();
    meter.prepare(&spec(RATE, 2));
    // Blocks of 10 ms, one for each slice of the meter.
    let mut block = AudioBuffer::<f64>::new(2, 480);
    let mut short_term = Vec::new();
    for index in 0..BLOCKS {
        let (left, right) = block.channel_pair_mut(0, 1);
        for (frame, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            let n = (index * 480 + frame) as f64;
            let level = if n < QUIET_FRAMES {
                quiet_from + (quiet_to - quiet_from) * n / QUIET_FRAMES
            } else {
                loud
            };
            let sample = 10f64.powf(level / 20.0) * (TAU * 1_000.0 * n / RATE).sin();
            *l = sample;
            *r = sample;
        }
        meter.measure(&block);
        let done = index + 1;
        if done >= 300 && (done - 300) % 10 == 0 {
            short_term.push(meter.readings().short_term());
        }
    }
    (meter.readings().range(), short_term)
}

#[test]
fn a_passage_astride_the_relative_gate_counts_as_a_whole_or_not_at_all() {
    // A quiet passage about 21.3 dB under a loud one sits on the relative
    // gate of the range. Its level creeps by 0.009 dB, so its hundred
    // short-term values spread over 0.007 LU: all in one bin of the
    // histogram, some above the gate and some below.
    let mut loud = -20.0;
    let mut quiet = loud - 21.34;
    let creep = 0.0091;
    let mut result = (0.0, Vec::new());
    // The first two passes only find where the gate and the passage fall
    // and move them to where they are wanted; the third is the measurement.
    for pass in 0..3 {
        let (range, short_term) =
            creeping_tone_then_loud(quiet - creep / 2.0, quiet + creep / 2.0, loud);
        let gate = tech_3342_gate(&short_term);
        let exact = tech_3342_range(&short_term, gate);
        let bin_start = ((gate + 70.0) * 100.0).floor() / 100.0 - 70.0;
        // The first 101 values lie wholly inside the quiet passage.
        let (lowest, highest) = (short_term[0], short_term[100]);
        println!(
            "pass {pass}: meter {range:.4} LU, Tech 3342 {exact:.4} LU; gate {gate:.5} LUFS, \
             {:.5} into its bin; quiet passage {lowest:.5} to {highest:.5} LUFS",
            gate - bin_start
        );
        result = (range, short_term);
        // Put the gate a little above the centre of its bin and the passage
        // around that centre.
        let shift = bin_start + 0.0054 - gate;
        loud += shift;
        quiet += shift + (bin_start + 0.005) - (0.5 * (lowest + highest) + shift);
    }
    let (range, short_term) = result;
    let gate = tech_3342_gate(&short_term);
    let exact = tech_3342_range(&short_term, gate);

    // The programme is what it was meant to be: the quiet passage shares
    // the gate's bin and lies on both sides of the gate.
    let bin_start = ((gate + 70.0) * 100.0).floor() / 100.0 - 70.0;
    let passage = &short_term[..=100];
    assert!(
        passage
            .iter()
            .all(|&loudness| (bin_start..bin_start + 0.01).contains(&loudness))
    );
    let above = passage.iter().filter(|&&loudness| loudness >= gate).count();
    println!("{above} of the 101 quiet values are at or above the gate");
    assert!(above > 30 && above < 70, "{above}");

    // By the function of Tech 3342 the values above the gate are enough to
    // hold the 10th percentile, and the range is the distance between the
    // two passages. The meter drops the passage whole, because the mean of
    // the bin is under the gate, and reads the range of the loud passage
    // alone. This is the limit that the documentation of `LoudnessMeter`
    // states; if the meter ever does better here, that text has to change
    // with these two lines.
    assert!((exact - 21.34).abs() < 0.01, "{exact}");
    assert!(range < 0.011, "{range}");

    // What the meter does promise: the range of that function for a gate
    // less than 0.01 LU from the true one. The function only changes when
    // its gate passes a value, so it is enough to try the gate at both ends
    // of that span and on every value inside it.
    let mut gates = vec![gate - 0.01, gate + 0.01];
    gates.extend(
        short_term
            .iter()
            .copied()
            .filter(|loudness| (loudness - gate).abs() < 0.01),
    );
    let (closest, moved) = gates
        .iter()
        .map(|&moved| ((range - tech_3342_range(&short_term, moved)).abs(), moved))
        .min_by(|one, other| one.0.total_cmp(&other.0))
        .expect("there are at least two gates to try");
    println!(
        "closest for a gate at {moved:.5} LUFS, {:+.5} LU from the true one: {closest:.4} LU apart",
        moved - gate
    );
    assert!(
        closest <= 0.0101,
        "the meter reads a loudness range of {range:.2} LU; no gate within 0.01 LU of the true \
         one makes the function of Tech 3342 give that for the short-term values the meter \
         itself reported (the closest is {closest:.2} LU away)"
    );

    // And the function has the same cliff, a hundredth of a loudness unit
    // away: with its gate that much higher it drops the passage too.
    assert!(tech_3342_range(&short_term, gate + 0.01) < 0.011);
}
