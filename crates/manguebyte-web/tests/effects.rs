//! Every effect of a part does what it says, measured through the engine:
//! a sampler part plays a known signal through the effect, and the output
//! is compared with what the effect should make of it, and with the curve
//! the engine draws for it.

mod common;

use common::*;
use manguebyte_web::{Chain, EFFECT_FADE_SECONDS, Effect, Engine, EventRecord, Visual, ids};

/// Runs `samples` through part 0 with `effect`, switched on with
/// `settings` on frame 0, and returns both channels of `frames` frames.
/// `None` runs it through no effect at all.
fn through(
    effect: Option<Effect>,
    settings: &[(&str, f64)],
    samples: &[f32],
    frames: usize,
) -> (Vec<f32>, Vec<f32>) {
    let (mut engine, records) = rigged(effect, settings, samples);
    push_all(&mut engine, &records);
    render(&mut engine, frames, 128)
}

/// The engine and the records [`through`] uses.
fn rigged(
    effect: Option<Effect>,
    settings: &[(&str, f64)],
    samples: &[f32],
) -> (Engine, Vec<EventRecord>) {
    let chain = effect.map_or(Chain::NONE, |effect| Chain::NONE.with(effect));
    let engine = signal_engine(samples, chain);
    let mut records: Vec<EventRecord> = settings
        .iter()
        .map(|&(name, value)| EventRecord::param(0, 0, id(name), value))
        .collect();
    if let Some(effect) = effect {
        records.push(EventRecord::param(
            0,
            0,
            id(&format!("{}.on", effect.name())),
            1.0,
        ));
    }
    records.push(EventRecord::note_on(0, 0, 60, 1.0));
    (engine, records)
}

fn db(ratio: f64) -> f64 {
    20.0 * ratio.log10()
}

/// The gain of `effect` at `settings` for a sine of `frequency` hertz, in
/// decibels: the level of the output against that of the same sine through
/// no effect, over half a second of steady state.
fn gain_at(effect: Effect, settings: &[(&str, f64)], frequency: f64) -> f64 {
    let tone = sine(frequency, RATE, 60_000, 0.5);
    let window = 24_000..48_000;
    let (wet, _) = through(Some(effect), settings, &tone, 48_000);
    let (dry, _) = through(None, &[], &tone, 48_000);
    db(rms(&wet[window.clone()]) / rms(&dry[window]))
}

/// The `y` of `curve` at `x`, read along straight lines between its points.
fn curve_at(points: &[[f32; 2]], x: f64) -> f64 {
    let x = x as f32;
    let next = points.iter().position(|point| point[0] >= x).unwrap();
    if next == 0 {
        return f64::from(points[0][1]);
    }
    let ([x0, y0], [x1, y1]) = (points[next - 1], points[next]);
    f64::from(y0 + (y1 - y0) * (x - x0) / (x1 - x0))
}

/// The curve the engine draws for `effect` of part 0 after `settings`.
fn curve_of(effect: Effect, settings: &[(&str, f64)], points: usize) -> Vec<[f32; 2]> {
    let (mut engine, records) = rigged(Some(effect), settings, &[0.0; 4]);
    push_all(&mut engine, &records);
    render(&mut engine, 1, 1);
    let curve = engine.effect_curve(0, effect, points).unwrap();
    assert_eq!(curve.visual, effect.visual());
    curve.points
}

/// An impulse: one sample at full scale.
fn impulse() -> Vec<f32> {
    let mut samples = vec![0.0; 8];
    samples[0] = 1.0;
    samples
}

#[test]
fn the_delay_echoes_on_time_and_each_echo_is_the_feedback_times_the_last() {
    let settings = [
        (ids::DELAY_TIME, 0.1),
        (ids::DELAY_FEEDBACK, 0.5),
        (ids::DELAY_MIX, 0.5),
        (ids::DELAY_DAMPING, 0.0),
    ];
    let (left, right) = through(Some(Effect::Delay), &settings, &impulse(), 30_000);
    let level = sampler_level();
    let period = 4_800;
    // The dry impulse at half level, then echoes of 0.5, 0.25, ... of it.
    assert_eq!(left[0], level * 0.5);
    let mut echoes = Vec::new();
    for (frame, &sample) in left.iter().enumerate().skip(1) {
        if sample != 0.0 {
            echoes.push((frame, sample));
        }
    }
    let expected: Vec<(usize, f32)> = (1..=6)
        .map(|echo| (echo * period, level * 0.5 * 0.5f32.powi(echo as i32 - 1)))
        .collect();
    assert_eq!(echoes.len(), expected.len(), "{echoes:?}");
    for (&(frame, sample), &(due, gain)) in echoes.iter().zip(&expected) {
        assert_eq!(frame, due);
        assert!(
            (sample - gain).abs() < 1e-7,
            "echo at {frame}: {sample} for {gain}"
        );
    }
    assert_eq!(left, right, "a sound in the centre echoes in the centre");

    // The taps the editor draws are the echoes measured.
    let taps = curve_of(Effect::Delay, &settings, 64);
    // Every echo down to -60 dB: 0.5 to the ninth is the last above it.
    assert_eq!(taps.len(), 9, "{taps:?}");
    for (&[time, gain], &(frame, sample)) in taps.iter().zip(&echoes) {
        assert!((f64::from(time) - frame as f64 / RATE).abs() < 1e-6);
        assert!((gain - sample / level).abs() < 1e-6, "{gain} {sample}");
    }
}

#[test]
fn a_synced_delay_follows_the_tempo_and_ping_pong_bounces_between_sides() {
    // Half a beat at 90 beats a minute: a third of a second.
    let settings = [
        (ids::MASTER_TEMPO, 90.0),
        (ids::DELAY_SYNC, 0.5),
        (ids::DELAY_TIME, 0.05),
        (ids::DELAY_FEEDBACK, 0.5),
        (ids::DELAY_MIX, 1.0),
        (ids::DELAY_DAMPING, 0.0),
        (ids::DELAY_PINGPONG, 1.0),
    ];
    let (left, right) = through(Some(Effect::Delay), &settings, &impulse(), 60_000);
    let level = sampler_level();
    let period = 16_000;
    let sounding = |channel: &[f32]| -> Vec<usize> {
        (0..channel.len()).filter(|&f| channel[f] != 0.0).collect()
    };
    // Fully wet: no dry impulse. The echoes go left, right, left.
    assert_eq!(sounding(&left), [period, 3 * period]);
    assert_eq!(sounding(&right), [2 * period]);
    assert!((left[period] - level).abs() < 1e-7);
    assert!((right[2 * period] - level * 0.5).abs() < 1e-7);
    assert!((left[3 * period] - level * 0.25).abs() < 1e-7);
    let taps = curve_of(Effect::Delay, &settings, 4);
    assert!((taps[0][0] - 1.0 / 3.0).abs() < 1e-6, "{taps:?}");
}

#[test]
fn damping_darkens_each_echo_and_keeps_its_low_end() {
    let settings = [
        (ids::DELAY_TIME, 0.1),
        (ids::DELAY_FEEDBACK, 0.6),
        (ids::DELAY_MIX, 1.0),
        (ids::DELAY_DAMPING, 0.7),
    ];
    let (left, _) = through(Some(Effect::Delay), &settings, &impulse(), 4_800 * 4);
    let level = f64::from(sampler_level());
    for echo in 1..=3usize {
        let span = &left[echo * 4_800..(echo + 1) * 4_800];
        // The damping is a low-pass: its gain at 0 Hz is 1, so each echo
        // adds up to the feedback times the last...
        let sum: f64 = span.iter().map(|&s| f64::from(s)).sum();
        let expected = level * 0.6f64.powi(echo as i32 - 1);
        assert!(
            (sum - expected).abs() < 1e-4 * expected,
            "echo {echo}: {sum} {expected}"
        );
        // ...while its peak falls faster: it is spread out, darker.
        assert!(f64::from(peak(span)) < 0.6 * expected, "echo {echo}");
    }
}

/// Upward zero crossings of `samples`, read between samples along a
/// straight line, from `from` on.
fn crossings(samples: &[f32], from: usize) -> Vec<f64> {
    (from.max(1)..samples.len())
        .filter(|&n| samples[n - 1] < 0.0 && samples[n] >= 0.0)
        .map(|n| {
            let (a, b) = (f64::from(samples[n - 1]), f64::from(samples[n]));
            (n - 1) as f64 + a / (a - b)
        })
        .collect()
}

/// How far a 50 Hz sine is delayed through `effect`, in milliseconds, at
/// every crossing over two seconds: the lowest and the highest.
fn delay_sweep(effect: Effect, settings: &[(&str, f64)]) -> (f64, f64) {
    let tone = sine(50.0, RATE, 3 * 48_000, 0.5);
    let (left, _) = through(Some(effect), settings, &tone, 3 * 48_000);
    let period = RATE / 50.0;
    let delays: Vec<f64> = crossings(&left, 24_000)
        .into_iter()
        .filter(|&at| at < 120_000.0)
        .map(|at| (at % period) * 1_000.0 / RATE)
        .collect();
    assert!(delays.len() > 90);
    let lowest = delays.iter().copied().fold(f64::INFINITY, f64::min);
    let highest = delays.iter().copied().fold(0.0, f64::max);
    (lowest, highest)
}

#[test]
fn the_chorus_and_the_flanger_sweep_their_delay_by_their_depth() {
    for (effect, settings, centre, depth) in [
        (
            Effect::Chorus,
            vec![
                (ids::CHORUS_MIX, 1.0),
                (ids::CHORUS_VOICES, 1.0),
                (ids::CHORUS_RATE, 0.5),
                (ids::CHORUS_DEPTH, 2.0),
                (ids::CHORUS_DELAY, 10.0),
                (ids::CHORUS_SPREAD, 0.0),
            ],
            10.0,
            2.0,
        ),
        (
            Effect::Flanger,
            vec![
                (ids::FLANGER_MIX, 1.0),
                (ids::FLANGER_RATE, 0.5),
                (ids::FLANGER_DEPTH, 1.5),
                (ids::FLANGER_DELAY, 3.0),
                (ids::FLANGER_FEEDBACK, 0.0),
            ],
            3.0,
            1.5,
        ),
    ] {
        let (lowest, highest) = delay_sweep(effect, &settings);
        eprintln!("{effect:?}: the delay swings from {lowest:.4} to {highest:.4} ms");
        assert!(
            (lowest - (centre - depth)).abs() < 0.02,
            "{effect:?} {lowest}"
        );
        assert!(
            (highest - (centre + depth)).abs() < 0.02,
            "{effect:?} {highest}"
        );
        // The cycle the editor draws spans the same.
        let cycle = curve_of(effect, &settings, 101);
        assert_eq!(cycle.first().unwrap()[0], 0.0);
        assert_eq!(cycle.last().unwrap()[0], 1.0);
        let low = cycle
            .iter()
            .map(|point| point[1])
            .fold(f32::INFINITY, f32::min);
        let high = cycle.iter().map(|point| point[1]).fold(0.0, f32::max);
        assert!((f64::from(low) - lowest).abs() < 0.02 && (f64::from(high) - highest).abs() < 0.02);
    }
}

#[test]
fn the_phaser_cuts_its_notches_where_the_formula_puts_them() {
    // Four stages held still at 1 kHz: two notches.
    let settings = [
        (ids::PHASER_DEPTH, 0.0),
        (ids::PHASER_CENTER, 1_000.0),
        (ids::PHASER_STAGES, 4.0),
        (ids::PHASER_FEEDBACK, 0.0),
        (ids::PHASER_MIX, 0.5),
    ];
    let tangent = (core::f64::consts::PI * 1_000.0 / RATE).tan();
    for k in 0..2 {
        let notch = RATE / core::f64::consts::PI
            * (tangent * ((2 * k + 1) as f64 * core::f64::consts::PI / 8.0).tan()).atan();
        let gain = gain_at(Effect::Phaser, &settings, notch);
        eprintln!("phaser notch {k} at {notch:.1} Hz: {gain:.1} dB");
        assert!(gain < -40.0, "{notch} Hz: {gain} dB");
    }
    // Far from the notches, the phaser at an equal mix leaves the level.
    assert!(gain_at(Effect::Phaser, &settings, 40.0).abs() < 0.5);
}

#[test]
fn the_equaliser_gives_each_band_its_gain_and_matches_its_curve() {
    let peak = [
        (ids::EQ_MID_GAIN, 6.0),
        (ids::EQ_MID_FREQ, 1_000.0),
        (ids::EQ_MID_Q, 1.0),
    ];
    let low = [(ids::EQ_LOW_GAIN, 6.0), (ids::EQ_LOW_FREQ, 120.0)];
    let high = [(ids::EQ_HIGH_GAIN, -6.0), (ids::EQ_HIGH_FREQ, 8_000.0)];
    let measured = [
        (gain_at(Effect::Eq, &peak, 1_000.0), 6.0),
        // A shelf is half its gain at its corner.
        (gain_at(Effect::Eq, &low, 120.0), 3.0),
        (gain_at(Effect::Eq, &high, 8_000.0), -3.0),
    ];
    eprintln!("EQ at the centres: {measured:?}");
    for (gain, expected) in measured {
        assert!((gain - expected).abs() < 0.1, "{gain} dB for {expected}");
    }

    // All three at once, against the curve the editor draws.
    let all = [
        (ids::EQ_LOW_GAIN, 4.0),
        (ids::EQ_LOW_FREQ, 150.0),
        (ids::EQ_MID_GAIN, -5.0),
        (ids::EQ_MID_FREQ, 2_000.0),
        (ids::EQ_MID_Q, 2.0),
        (ids::EQ_HIGH_GAIN, 3.0),
        (ids::EQ_HIGH_FREQ, 6_000.0),
    ];
    let curve = curve_of(Effect::Eq, &all, 512);
    for frequency in [50.0, 150.0, 500.0, 2_000.0, 3_000.0, 8_000.0, 12_000.0] {
        let gain = gain_at(Effect::Eq, &all, frequency);
        let drawn = curve_at(&curve, frequency);
        assert!(
            (gain - drawn).abs() < 0.1,
            "{frequency} Hz: {gain} measured, {drawn} drawn"
        );
    }
}

#[test]
fn the_filters_match_their_curves() {
    for (effect, settings) in [
        (
            Effect::Highpass,
            [
                (ids::HIGHPASS_CUTOFF, 1_000.0),
                (ids::HIGHPASS_RESONANCE, 0.3),
            ],
        ),
        (
            Effect::Lowpass,
            [
                (ids::LOWPASS_CUTOFF, 2_000.0),
                (ids::LOWPASS_RESONANCE, 0.5),
            ],
        ),
    ] {
        let curve = curve_of(effect, &settings, 512);
        for frequency in [100.0, 500.0, 1_000.0, 2_000.0, 6_000.0] {
            let gain = gain_at(effect, &settings, frequency);
            let drawn = curve_at(&curve, frequency);
            assert!(
                (gain - drawn).abs() < 0.1,
                "{effect:?} at {frequency} Hz: {gain} measured, {drawn} drawn"
            );
        }
    }
}

/// A square wave of 100 Hz with peaks of `amplitude`: a level that stays.
fn square(amplitude: f32, frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|n| {
            if (n / 240) % 2 == 0 {
                amplitude
            } else {
                -amplitude
            }
        })
        .collect()
}

#[test]
fn the_compressor_reduces_by_its_ratio_over_the_threshold() {
    let settings = [
        (ids::COMPRESSOR_THRESHOLD, -18.0),
        (ids::COMPRESSOR_RATIO, 4.0),
        (ids::COMPRESSOR_KNEE, 0.0),
        (ids::COMPRESSOR_ATTACK, 1.0),
        (ids::COMPRESSOR_RELEASE, 50.0),
    ];
    let curve = curve_of(Effect::Compressor, &settings, 661);
    for amplitude in [1.0f32, 0.5, 0.25, 0.1] {
        let input = db(f64::from(amplitude * sampler_level()));
        let (left, _) = through(
            Some(Effect::Compressor),
            &settings,
            &square(amplitude, 30_000),
            24_000,
        );
        let output = db(f64::from(peak(&left[12_000..24_000])));
        let over = (input + 18.0).max(0.0);
        let expected = input - over * 0.75;
        eprintln!("compressor: {input:.2} dBFS in, {output:.2} out");
        assert!(
            (output - expected).abs() < 0.1,
            "{input}: {output} for {expected}"
        );
        assert!((curve_at(&curve, input) - output).abs() < 0.1);
    }
}

#[test]
fn the_bitcrusher_rounds_to_its_steps_and_holds_at_its_rate() {
    // A slow ramp over the whole range: every step of the staircase.
    let ramp: Vec<f32> = (0..48_000).map(|n| n as f32 / 24_000.0 - 1.0).collect();
    let settings = [(ids::BITCRUSH_BITS, 4.0), (ids::BITCRUSH_RATE, 6_000.0)];
    let (left, right) = through(Some(Effect::Bitcrush), &settings, &ramp, 50_000);
    assert_eq!(left, right);
    let step = 1.0 / 8.0;
    let mut levels: Vec<f32> = left[..48_000].to_vec();
    levels.sort_by(f32::total_cmp);
    levels.dedup();
    // Four bits over -1 to 1, of a signal at half scale: nine steps.
    assert_eq!(levels.len(), 9, "{levels:?}");
    for (index, &level) in levels.iter().enumerate() {
        assert_eq!(level, (index as f32 - 4.0) * step);
    }
    // Held for 8 frames at a time: 48 kHz over 6 kHz.
    let changes: Vec<usize> = (1..48_000).filter(|&n| left[n] != left[n - 1]).collect();
    assert!(changes.iter().all(|&n| n % 8 == 0), "{changes:?}");
    // Silence stays exact silence: no offset.
    assert!(left[48_000..].iter().all(|&sample| sample == 0.0));
    // The staircase the editor draws.
    let steps = curve_of(Effect::Bitcrush, &settings, 1_001);
    for &[x, y] in &steps {
        assert_eq!(y, (x / step).round() * step);
    }
}

#[test]
fn the_drive_bends_the_signal_as_its_curve_says() {
    let ramp: Vec<f32> = (0..48_000).map(|n| n as f32 / 24_000.0 - 1.0).collect();
    for shape in 0..4 {
        let settings = [
            (ids::DRIVE_AMOUNT, 18.0),
            (ids::DRIVE_OUTPUT, -3.0),
            (ids::DRIVE_SHAPE, f64::from(shape)),
        ];
        let (left, _) = through(Some(Effect::Drive), &settings, &ramp, 48_000);
        let curve = curve_of(Effect::Drive, &settings, 4_001);
        let level = sampler_level();
        for n in (0..48_000).step_by(997) {
            let x = f64::from(ramp[n] * level);
            let drawn = curve_at(&curve, x);
            assert!(
                (f64::from(left[n]) - drawn).abs() < 0.01,
                "shape {shape} at {x}: {} measured, {drawn} drawn",
                left[n]
            );
        }
    }
}

#[test]
fn the_tremolo_and_the_auto_pan_follow_their_waves() {
    let steady = vec![1.0f32; 3 * 48_000];
    let level = f64::from(sampler_level());
    // A tremolo of full depth at 2 Hz: the gain follows the drawn cycle.
    let tremolo = [(ids::TREMOLO_RATE, 2.0), (ids::TREMOLO_DEPTH, 1.0)];
    let (left, _) = through(Some(Effect::Tremolo), &tremolo, &steady, 96_000);
    let cycle = curve_of(Effect::Tremolo, &tremolo, 1_001);
    for n in (2_000..96_000).step_by(500) {
        let phase = (n as f64 * 2.0 / RATE).fract();
        let drawn = curve_at(&cycle, phase);
        let gain = f64::from(left[n]) / level;
        // The gain follows its wave through a smoother of 1 ms.
        assert!((gain - drawn).abs() < 0.02, "{n}: {gain} {drawn}");
    }
    assert!(f64::from(peak(&left[48_000..96_000])) > 0.999 * level);
    let lowest = left[48_000..96_000]
        .iter()
        .fold(1.0f32, |low, &s| low.min(s));
    assert!(f64::from(lowest) < 0.001 * level, "{lowest}");

    // Locked to one beat a cycle at 120 beats a minute, it is the same wave.
    let synced = [
        (ids::TREMOLO_SYNC, 1.0),
        (ids::TREMOLO_RATE, 7.0),
        (ids::TREMOLO_DEPTH, 1.0),
    ];
    let (locked, _) = through(Some(Effect::Tremolo), &synced, &steady, 96_000);
    for (n, (&free, &locked)) in left.iter().zip(&locked).enumerate() {
        assert!((free - locked).abs() < 1e-4, "{n}: {free} {locked}");
    }

    // The auto-pan moves the sound from side to side at constant power.
    let autopan = [(ids::AUTOPAN_RATE, 2.0), (ids::AUTOPAN_DEPTH, 1.0)];
    let (left, right) = through(Some(Effect::Autopan), &autopan, &steady, 96_000);
    for n in (2_000..96_000).step_by(250) {
        let power = f64::from(left[n]).powi(2) + f64::from(right[n]).powi(2);
        assert!((power - 2.0 * level * level).abs() < 1e-3, "{n}: {power}");
    }
    assert!(f64::from(peak(&right)) > 1.41 * level, "hard right");
    assert!(left.iter().any(|&sample| sample < 1e-3), "and nothing left");
}

/// A sine of 220 Hz at 0.4 for three seconds: steps of at most 0.0115.
fn hum() -> Vec<f32> {
    sine(220.0, RATE, 3 * 48_000, 0.8)
}

/// The largest step from one sample to the next in `left[range]` and
/// `right[range]`.
fn steps(left: &[f32], right: &[f32], range: core::ops::Range<usize>) -> f32 {
    largest_step(&left[range.clone()]).max(largest_step(&right[range]))
}

/// Every effect switched in, changed, and switched out while a tone
/// plays: no step from one sample to the next while it fades or glides is
/// larger than the largest step of the steady sound before and after, by
/// more than 2% of the tone's peak.
#[test]
fn switching_and_changing_effects_never_clicks() {
    let fade = (EFFECT_FADE_SECONDS * RATE) as usize;
    let tone = hum();
    let level = sampler_level() * 0.8;
    let cases: [(Effect, &[(&str, f64)]); 13] = [
        (
            Effect::Highpass,
            &[
                (ids::HIGHPASS_CUTOFF, 2_000.0),
                (ids::HIGHPASS_RESONANCE, 0.8),
            ],
        ),
        (
            Effect::Lowpass,
            &[(ids::LOWPASS_CUTOFF, 300.0), (ids::LOWPASS_RESONANCE, 1.0)],
        ),
        (
            Effect::Eq,
            &[
                (ids::EQ_MID_GAIN, -24.0),
                (ids::EQ_MID_FREQ, 220.0),
                (ids::EQ_LOW_GAIN, 12.0),
            ],
        ),
        (
            Effect::Drive,
            &[(ids::DRIVE_AMOUNT, 30.0), (ids::DRIVE_SHAPE, 3.0)],
        ),
        (
            Effect::Bitcrush,
            &[(ids::BITCRUSH_BITS, 3.0), (ids::BITCRUSH_MIX, 0.2)],
        ),
        (
            Effect::Compressor,
            &[
                (ids::COMPRESSOR_THRESHOLD, -40.0),
                (ids::COMPRESSOR_MAKEUP, 12.0),
            ],
        ),
        (
            Effect::Chorus,
            &[
                (ids::CHORUS_DEPTH, 8.0),
                (ids::CHORUS_VOICES, 8.0),
                (ids::CHORUS_DELAY, 30.0),
            ],
        ),
        (
            Effect::Flanger,
            &[(ids::FLANGER_FEEDBACK, -0.9), (ids::FLANGER_DELAY, 8.0)],
        ),
        (
            Effect::Phaser,
            &[
                (ids::PHASER_STAGES, 12.0),
                (ids::PHASER_CENTER, 300.0),
                (ids::PHASER_FEEDBACK, 0.9),
            ],
        ),
        (
            Effect::Tremolo,
            &[(ids::TREMOLO_DEPTH, 1.0), (ids::TREMOLO_RATE, 9.0)],
        ),
        (
            Effect::Autopan,
            &[(ids::AUTOPAN_DEPTH, 1.0), (ids::AUTOPAN_RATE, 5.0)],
        ),
        (
            Effect::Delay,
            &[
                (ids::DELAY_TIME, 0.31),
                (ids::DELAY_FEEDBACK, 0.9),
                (ids::DELAY_PINGPONG, 1.0),
            ],
        ),
        (
            Effect::Reverb,
            &[(ids::REVERB_MIX, 0.9), (ids::REVERB_DECAY, 6.0)],
        ),
    ];
    for (effect, changes) in cases {
        let (on, change, off) = (24_000u64, 48_000u64, 96_000u64);
        let mut engine = signal_engine(&tone, Chain::NONE.with(effect));
        let switch = id(&format!("{}.on", effect.name()));
        let mut records = vec![
            EventRecord::param(0, 0, switch, 0.0),
            EventRecord::note_on(0, 0, 60, 1.0),
            EventRecord::param(on, 0, switch, 1.0),
            EventRecord::param(off, 0, switch, 0.0),
        ];
        for &(name, value) in changes {
            records.push(EventRecord::param(change, 0, id(name), value));
        }
        push_all(&mut engine, &records);
        let (left, right) = render(&mut engine, 120_000, 128);
        let before = steps(&left, &right, 12_000..on as usize);
        let worst = [
            (
                on as usize,
                steps(&left, &right, on as usize..on as usize + 2 * fade),
            ),
            (
                change as usize,
                steps(&left, &right, change as usize..change as usize + 4_800),
            ),
            (
                off as usize,
                steps(&left, &right, off as usize..off as usize + 2 * fade),
            ),
        ];
        let steady_on = steps(&left, &right, on as usize + 4_800..change as usize);
        let steady_changed = steps(&left, &right, change as usize + 9_600..off as usize);
        let after = steps(&left, &right, off as usize + 4_800..120_000);
        let allowed = before.max(steady_on).max(steady_changed).max(after) + 0.02 * level;
        eprintln!(
            "{effect:?}: steady {before:.4} {steady_on:.4} {steady_changed:.4} {after:.4}, switching {:.4} {:.4} {:.4}",
            worst[0].1, worst[1].1, worst[2].1
        );
        for (at, step) in worst {
            assert!(
                step <= allowed,
                "{effect:?} at {at}: a step of {step}, {allowed} allowed"
            );
        }
    }
}

/// An effect in the chain but switched off costs nothing: it is skipped,
/// and the sound is exactly what it is without the effect.
#[test]
fn effects_cost_nothing_until_they_are_switched_on() {
    let tone = hum();
    let mut plain = signal_engine(&tone, Chain::NONE);
    let mut equipped = signal_engine(&tone, Chain::ALL);
    let records = [
        EventRecord::param(0, 0, id(ids::LOWPASS_ON), 0.0),
        EventRecord::param(0, 0, id(ids::REVERB_ON), 0.0),
        EventRecord::note_on(0, 0, 60, 1.0),
    ];
    push_all(&mut plain, &records);
    push_all(&mut equipped, &records);
    let (a, _) = render(&mut plain, 24_000, 128);
    let (b, _) = render(&mut equipped, 24_000, 128);
    assert_eq!(bits(&a), bits(&b));
    for effect in Effect::PART_ORDER {
        assert_eq!(
            equipped.effect_running(0, effect),
            Some(false),
            "{effect:?}"
        );
    }
    // The low-pass and the reverb start switched on, as they always did.
    let fresh = signal_engine(&tone, Chain::ALL);
    let running: Vec<Effect> = Effect::PART_ORDER
        .into_iter()
        .filter(|&effect| fresh.effect_running(0, effect) == Some(true))
        .collect();
    assert_eq!(running, [Effect::Lowpass, Effect::Reverb]);
    assert_eq!(fresh.effect_running(9, Effect::Delay), None);
}

/// The samples `records` make through `chain`, both sides, in quanta of
/// 1, 7, 128 and 512 frames: `None` if they are all the same, bit for bit,
/// or the first quantum size that differs from single frames.
fn quantum_that_differs(chain: Chain, records: &[EventRecord], frames: usize) -> Option<usize> {
    let tone = hum();
    let runs: Vec<(usize, Vec<u32>)> = [1, 7, 128, 512]
        .into_iter()
        .map(|quantum| {
            let mut engine = signal_engine(&tone, chain);
            push_all(&mut engine, records);
            let (left, right) = render(&mut engine, frames, quantum);
            let mut both = bits(&left);
            both.extend(bits(&right));
            (quantum, both)
        })
        .collect();
    for (quantum, run) in &runs[1..] {
        if let Some(at) = run.iter().zip(&runs[0].1).position(|(a, b)| a != b) {
            eprintln!(
                "quantum {quantum}: first difference at {at}: {} {}",
                f32::from_bits(run[at]),
                f32::from_bits(runs[0].1[at])
            );
        }
    }
    runs[1..]
        .iter()
        .find(|(_, run)| *run != runs[0].1)
        .map(|&(quantum, _)| quantum)
}

/// Each effect, and every effect at once, gives the same samples, bit for
/// bit, however the frames are cut into quanta.
#[test]
fn every_effect_is_the_same_in_every_quantum() {
    for effect in Effect::PART_ORDER {
        let switch = id(&format!("{}.on", effect.name()));
        let records = [
            EventRecord::note_on(0, 0, 60, 1.0),
            EventRecord::param(3_000, 0, switch, 1.0),
            EventRecord::param(11_456, 0, switch, 0.0),
            EventRecord::param(16_001, 0, switch, 1.0),
        ];
        assert_eq!(
            quantum_that_differs(Chain::NONE.with(effect), &records, 24_000),
            None,
            "{effect:?}"
        );
    }
    let mut records = vec![EventRecord::note_on(0, 0, 60, 1.0)];
    for effect in Effect::PART_ORDER {
        records.push(EventRecord::param(
            0,
            0,
            id(&format!("{}.on", effect.name())),
            1.0,
        ));
    }
    for (frame, name, value) in [
        (0u64, ids::DELAY_PINGPONG, 0.5),
        (0, ids::DELAY_TIME, 0.123),
        (0, ids::EQ_MID_GAIN, 4.0),
        (0, ids::COMPRESSOR_THRESHOLD, -30.0),
        (0, ids::BITCRUSH_RATE, 11_025.0),
        (0, ids::TREMOLO_SYNC, 0.25),
        (0, ids::SEND, 0.5),
        (0, ids::GLUE_ON, 1.0),
        (7_001, ids::EQ_LOW_GAIN, -9.0),
        (9_999, ids::DELAY_TIME, 0.2),
        (10_000, ids::MASTER_TEMPO, 133.0),
        (12_345, ids::DRIVE_SHAPE, 2.0),
        (20_000, ids::CHORUS_ON, 0.0),
        (30_003, ids::PHASER_STAGES, 9.0),
        (40_000, ids::DELAY_ON, 0.0),
    ] {
        records.push(EventRecord::param(frame, 0, id(name), value));
    }
    assert_eq!(quantum_that_differs(Chain::ALL, &records, 60_000), None);
}

/// A sample that is not a number, played through every effect, silences
/// the part for that stretch and clears every effect: nothing sticks, and
/// the next note plays through them all again.
#[test]
fn a_sample_that_is_not_a_number_does_not_stick_in_any_effect() {
    for poison in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut samples = sine(220.0, RATE, 9_600, 0.5);
        samples[4_000] = poison;
        let mut engine = signal_engine(&samples, Chain::ALL);
        let mut records = vec![EventRecord::note_on(0, 0, 60, 1.0)];
        for effect in Effect::PART_ORDER {
            records.push(EventRecord::param(
                0,
                0,
                id(&format!("{}.on", effect.name())),
                1.0,
            ));
        }
        records.push(EventRecord::param(0, 0, id(ids::SEND), 0.7));
        // Struck again later, clean this time up to the poison.
        records.push(EventRecord::note_on(30_000, 0, 60, 1.0));
        push_all(&mut engine, &records);
        let (left, right) = render(&mut engine, 33_000, 128);
        assert!(left.iter().chain(&right).all(|sample| sample.is_finite()));
        let recovered = engine.stats().recovered;
        assert!(recovered >= 1, "{poison}: {recovered}");
        // Before the poison came out, and after the second note, sound.
        assert!(peak(&left[1_000..3_900]) > 0.01);
        assert!(
            peak(&left[30_200..33_000]) > 0.01,
            "{poison}: the effects play again"
        );
        // The shared reverb, fed by the send, is finite too.
        assert!(!engine.master_reverb_asleep() || peak(&left[30_200..]) > 0.0);
    }
}

/// Every value an event can carry, for every parameter of every effect and
/// of the master bus, in a random order, with every effect switched in and
/// out as it comes: nothing produces a sample that is not a finite number,
/// and the output stays under the ceiling.
#[test]
fn no_value_an_event_can_carry_makes_an_effect_go_wrong() {
    let extremes = [
        f64::MAX,
        -f64::MAX,
        1e300,
        -1e300,
        f64::MIN_POSITIVE,
        0.0,
        -0.0,
        0.5,
        1.0,
        12.0,
        -60.0,
        20_000.0,
    ];
    let mut seed = 0xBB67_AE85_84CA_A73Bu64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut engine = engine();
    engine
        .create_part_with(0, manguebyte_web::InstrumentKind::Synth, Chain::ALL)
        .unwrap();
    engine
        .create_part_with(1, manguebyte_web::InstrumentKind::Drums, Chain::ALL)
        .unwrap();
    let effect_ids: Vec<&str> = ids::ALL
        .iter()
        .copied()
        .filter(|name| name.contains('.') && !name.starts_with("synth") && !name.contains("level"))
        .chain(ids::MASTER)
        .collect();
    for frame in (0..60_000u64).step_by(200) {
        let part = (next() % 2) as u16;
        for _ in 0..4 {
            let name = effect_ids[(next() % effect_ids.len() as u64) as usize];
            let value = extremes[(next() % extremes.len() as u64) as usize];
            engine.push(EventRecord::param(frame, part, id(name), value));
        }
        let note = 30 + (next() % 70) as u8;
        engine.push(EventRecord::note_on(frame, part, note, 1.0));
        engine.push(EventRecord::note_off(frame + 150, part, note));
    }
    let (left, right) = render(&mut engine, 64_000, 512);
    let stats = engine.stats();
    assert_eq!(stats.recovered, 0, "{stats:?}");
    assert_eq!(stats.dropped, 0);
    assert!(peak(&left) > 0.01, "nothing sounded");
    for sample in left.iter().chain(&right) {
        assert!(sample.is_finite() && sample.abs() <= manguebyte_web::OUTPUT_CEILING);
    }
}

#[test]
fn every_part_effect_has_a_curve_of_its_visual() {
    let engine = signal_engine(&[0.0; 4], Chain::NONE);
    for effect in Effect::PART_ORDER {
        let curve = engine.effect_curve(0, effect, 64).unwrap();
        assert_eq!(curve.visual, effect.visual());
        assert!(!curve.points.is_empty());
        assert!(
            curve.points.iter().flatten().all(|value| value.is_finite()),
            "{effect:?}"
        );
        assert!(curve.points.windows(2).all(|pair| pair[0][0] <= pair[1][0]));
    }
    assert_eq!(engine.effect_curve(0, Effect::Glue, 64), None);
    assert_eq!(engine.effect_curve(1, Effect::Delay, 64), None);
    assert_eq!(
        engine.effect_curve(0, Effect::Eq, 1).unwrap().points.len(),
        2,
        "at least two points"
    );
    let visuals: Vec<&str> = Effect::ALL
        .iter()
        .map(|effect| effect.visual().name())
        .collect();
    for name in [
        "eq_curve",
        "filter_curve",
        "transfer_curve",
        "lfo_shape",
        "delay_taps",
        "reverb_decay",
        "bitcrush_steps",
    ] {
        assert!(visuals.contains(&name), "{name}");
    }
    assert_eq!(Visual::DelayTaps.name(), "delay_taps");
}

/// Asserts that every point of `points` is at `x`, `y` with the `y` that
/// `expected` gives for `x`, to the precision of an `f32`.
fn assert_curve(name: &str, points: &[[f32; 2]], xs: &[f64], expected: impl Fn(f64) -> f64) {
    assert_eq!(points.len(), xs.len(), "{name}");
    for (&[x, y], &at) in points.iter().zip(xs) {
        assert_eq!(x, at as f32, "{name}");
        let want = expected(at);
        assert!(
            (f64::from(y) - want).abs() <= 1e-5 * want.abs().max(1.0),
            "{name} at {at}: {y} against {want}"
        );
    }
}

/// The curves are `manguebyte-dsp`'s own response curves of the settings
/// the parameters hold, worked out from the same coefficients and limits
/// the effects run on.
#[test]
fn the_curves_are_those_of_manguebyte_dsp() {
    use manguebyte_dsp::curve::{
        BiquadResponse, ChorusSweep, CompressorTransfer, Curve as _, DelayTaps, FrequencyResponse,
        LfoCycle, LimiterTransfer, PhaserSweep, ReverbDecay, SvfResponse, linear_spaced,
        log_spaced,
    };
    use manguebyte_dsp::{FilterShape, LfoShape, ShaperCurve, SvfMode};

    let points = 97;
    let mut hertz = vec![0.0; points];
    log_spaced(20.0, 20_000.0, &mut hertz);
    let mut levels = vec![0.0; points];
    linear_spaced(-60.0, 6.0, &mut levels);
    let mut samples = vec![0.0; points];
    linear_spaced(-1.0, 1.0, &mut samples);
    let mut cycle = vec![0.0; points];
    linear_spaced(0.0, 1.0, &mut cycle);
    let in_cycle = |phase: f64| phase.min(1.0 - 1e-12);
    // The filters' resonance as their Q, as the README gives it.
    let q = |resonance: f64| core::f64::consts::FRAC_1_SQRT_2 * 16f64.powf(resonance);
    let f32_of = |value: f64| f64::from(value as f32);

    let filters = [
        (
            Effect::Lowpass,
            SvfMode::Lowpass,
            ids::LOWPASS_CUTOFF,
            ids::LOWPASS_RESONANCE,
        ),
        (
            Effect::Highpass,
            SvfMode::Highpass,
            ids::HIGHPASS_CUTOFF,
            ids::HIGHPASS_RESONANCE,
        ),
    ];
    for (effect, mode, cutoff, resonance) in filters {
        let curve = curve_of(effect, &[(cutoff, 1_234.5), (resonance, 0.6)], points);
        let filter = SvfResponse::new(mode, f32_of(1_234.5), q(f32_of(0.6)), RATE);
        assert_curve(effect.name(), &curve, &hertz, |f| filter.magnitude_db_at(f));
    }

    let eq = [
        (ids::EQ_LOW_GAIN, 4.0),
        (ids::EQ_LOW_FREQ, 150.0),
        (ids::EQ_MID_GAIN, -7.5),
        (ids::EQ_MID_FREQ, 2_000.0),
        (ids::EQ_MID_Q, 2.5),
        (ids::EQ_HIGH_GAIN, 3.0),
        (ids::EQ_HIGH_FREQ, 9_000.0),
    ];
    let curve = curve_of(Effect::Eq, &eq, points);
    let shelf = core::f64::consts::FRAC_1_SQRT_2;
    let bands = [
        BiquadResponse::design(FilterShape::LowShelf { gain_db: 4.0 }, 150.0, shelf, RATE),
        BiquadResponse::design(FilterShape::Peak { gain_db: -7.5 }, 2_000.0, 2.5, RATE),
        BiquadResponse::design(
            FilterShape::HighShelf { gain_db: 3.0 },
            9_000.0,
            shelf,
            RATE,
        ),
    ];
    assert_curve("eq", &curve, &hertz, |f| bands.magnitude_db_at(f));

    let compressor = [
        (ids::COMPRESSOR_THRESHOLD, -24.0),
        (ids::COMPRESSOR_RATIO, 5.0),
        (ids::COMPRESSOR_KNEE, 8.0),
        (ids::COMPRESSOR_MAKEUP, 3.0),
    ];
    let curve = curve_of(Effect::Compressor, &compressor, points);
    let transfer = CompressorTransfer::new(-24.0, 5.0, 8.0, 3.0);
    assert_curve("compressor", &curve, &levels, |level| {
        transfer.value_at(level)
    });

    let drive = [
        (ids::DRIVE_AMOUNT, 0.0),
        (ids::DRIVE_OUTPUT, 0.0),
        (ids::DRIVE_SHAPE, 1.0),
    ];
    let curve = curve_of(Effect::Drive, &drive, points);
    assert_curve("drive", &curve, &samples, |x| {
        ShaperCurve::SoftClip.value_at(x)
    });

    let chorus = [
        (ids::CHORUS_DELAY, 12.0),
        (ids::CHORUS_DEPTH, 4.0),
        (ids::CHORUS_RATE, 1.5),
    ];
    let curve = curve_of(Effect::Chorus, &chorus, points);
    let sweep = ChorusSweep::new(12.0, 4.0, 1.5, RATE);
    assert_curve("chorus", &curve, &cycle, |phase| {
        sweep.value_at(in_cycle(phase))
    });

    let flanger = [(ids::FLANGER_DELAY, 1.0), (ids::FLANGER_DEPTH, 2.0)];
    let curve = curve_of(Effect::Flanger, &flanger, points);
    let sweep = ChorusSweep::new(1.0, 2.0, f32_of(0.25), RATE);
    assert_curve("flanger", &curve, &cycle, |phase| {
        sweep.value_at(in_cycle(phase))
    });

    let phaser = [(ids::PHASER_CENTER, 700.0), (ids::PHASER_DEPTH, 3.0)];
    let curve = curve_of(Effect::Phaser, &phaser, points);
    let sweep = PhaserSweep::new(700.0, 3.0, 0.5, RATE);
    assert_curve("phaser", &curve, &cycle, |phase| {
        sweep.value_at(in_cycle(phase))
    });

    let tremolo = [(ids::TREMOLO_DEPTH, 0.75), (ids::TREMOLO_SHAPE, 1.0)];
    let curve = curve_of(Effect::Tremolo, &tremolo, points);
    let wave = LfoCycle::new(LfoShape::Triangle);
    assert_curve("tremolo", &curve, &cycle, |phase| {
        let value = wave.value_at(in_cycle(phase));
        1.0 - 0.75 * (1.0 - (value + 1.0) / 2.0)
    });

    // A delay of 0.2 s: its echoes, and with ping-pong part of the way the
    // louder side's.
    for pingpong in [0.0, 0.5, 0.8, 1.0] {
        let settings = [
            (ids::DELAY_TIME, 0.2),
            (ids::DELAY_FEEDBACK, 0.6),
            (ids::DELAY_MIX, 0.5),
            (ids::DELAY_PINGPONG, pingpong),
        ];
        let curve = curve_of(Effect::Delay, &settings, 64);
        let taps = DelayTaps::new(0.2, f32_of(0.6), 0.5, RATE);
        assert!(curve.len() > 5);
        let p = f32_of(pingpong);
        let louder = |echo: usize| {
            let spread = 1.0 - 0.5 * p + 0.5 * p * (1.0 - 2.0 * p).abs().powi(echo as i32 - 1);
            taps.tap(echo).y * spread
        };
        for (index, &[time, gain]) in curve.iter().enumerate() {
            let echo = index + 1;
            assert_eq!(time, taps.tap(echo).x as f32);
            assert!(
                (f64::from(gain) - louder(echo)).abs() < 1e-6,
                "{pingpong} {echo}"
            );
            assert!(louder(echo) >= 1e-3);
        }
        assert!(
            louder(curve.len() + 1) < 1e-3,
            "every echo above -60 dB, and no more"
        );
    }

    let reverb = [(ids::REVERB_DECAY, 2.0), (ids::REVERB_MIX, 0.5)];
    let curve = curve_of(Effect::Reverb, &reverb, points);
    let decay = ReverbDecay::new(2.0, 0.45, 0.6, RATE).energy_at(1_000.0);
    let mut times = vec![0.0; points];
    linear_spaced(0.0, 3.0, &mut times);
    assert_curve("reverb", &curve, &times, |t| db(0.5) + decay.value_at(t));

    // The master bus's.
    let mut engine = signal_engine(&[0.0; 4], Chain::NONE);
    push_all(
        &mut engine,
        &[
            EventRecord::param(0, 0, id(ids::GLUE_THRESHOLD), -20.0),
            EventRecord::param(0, 0, id(ids::GLUE_RATIO), 3.0),
            EventRecord::param(0, 0, id(ids::LIMITER_CEILING), -2.0),
            EventRecord::param(0, 0, id(ids::MASTER_REVERB_DECAY), 3.0),
            EventRecord::param(0, 0, id(ids::MASTER_REVERB_DAMPING), 0.8),
        ],
    );
    render(&mut engine, 1, 1);
    let curve = engine.master_curve(Effect::Glue, points).unwrap().points;
    let transfer = CompressorTransfer::new(-20.0, 3.0, 6.0, 0.0);
    assert_curve("glue", &curve, &levels, |level| transfer.value_at(level));
    let curve = engine.master_curve(Effect::Limiter, points).unwrap().points;
    let transfer = LimiterTransfer::new(-2.0);
    assert_curve("limiter", &curve, &levels, |level| transfer.value_at(level));
    let curve = engine.master_curve(Effect::Reverb, points).unwrap().points;
    let decay = ReverbDecay::new(3.0, f32_of(0.8), f32_of(0.7), RATE).energy_at(1_000.0);
    linear_spaced(0.0, 4.5, &mut times);
    assert_curve("master reverb", &curve, &times, |t| decay.value_at(t));
}
