//! The curves an editor draws for an effect, worked out from its
//! parameters.
//!
//! Every function here reads the parameters as they stand (the values the
//! last events set, not the smoothed values a block is processing with)
//! and computes the curve in closed form, outside the render call, from
//! the same formulas the effects run on: the filters' magnitude from their
//! biquad coefficients ([`BiquadCoefficients::magnitude_at`]), the
//! compressor's static curve ([`Compressor::static_reduction_db`]), the
//! waveshaper's and the bitcrusher's transfer, one cycle of an [`Lfo`], the
//! echoes of an impulse, the decay of a reverb.
//!
//! These are meant to be replaced by the response-curve functions
//! `manguebyte-dsp` is getting: the swap happens in [`part_curve`] and
//! [`master_curve`], one match arm per effect, and nothing outside this
//! file changes, as long as the new functions keep the axes of
//! [`Visual`].

use manguebyte_dsp::{BiquadCoefficients, Compressor, FilterShape, Lfo};
use manguebyte_params::Param;

use crate::effect::{Effect, Visual};
use crate::fx::{
    Equaliser, crush, crush_step, drive_curve, gain_from_db, lfo_shape, resonance_to_q,
    wobble_target,
};
use crate::params::{ChainParams, MasterParams};

/// The most points a curve has.
pub const MAX_CURVE_POINTS: usize = 4_096;

/// The points an editor draws for an effect, `[x, y]` each, in the axes
/// its [`Visual`] gives.
#[derive(Debug, Clone, PartialEq)]
pub struct Curve {
    /// What the points are a picture of.
    pub visual: Visual,
    /// The points, in increasing `x`.
    pub points: Vec<[f32; 2]>,
}

/// Lowest and highest input level of a compressor's or a limiter's
/// transfer curve, dBFS.
const LEVELS_DB: (f64, f64) = (-60.0, 6.0);

/// Lowest frequency of a frequency response, hertz.
const LOWEST_HZ: f64 = 20.0;

/// An echo quieter than this, -60 dB, ends a delay's taps.
const LAST_TAP: f64 = 1e-3;

fn db(gain: f64) -> f64 {
    20.0 * gain.max(1e-12).log10()
}

/// `points` values from `from` to `to`, evenly spaced.
fn linear(from: f64, to: f64, points: usize) -> impl Iterator<Item = f64> {
    let last = (points - 1).max(1) as f64;
    (0..points).map(move |index| from + (to - from) * index as f64 / last)
}

/// `points` frequencies from 20 Hz to 20 kHz or just under half the
/// sample rate, evenly spaced in octaves.
fn frequencies(sample_rate: f64, points: usize) -> impl Iterator<Item = f64> {
    let highest = (0.49 * sample_rate).min(20_000.0);
    linear(LOWEST_HZ.log2(), highest.log2(), points).map(f64::exp2)
}

fn response(
    sample_rate: f64,
    points: usize,
    bands: &[BiquadCoefficients],
) -> impl Iterator<Item = [f32; 2]> {
    frequencies(sample_rate, points).map(move |frequency| {
        let gain: f64 = bands
            .iter()
            .map(|band| band.magnitude_at(frequency, sample_rate))
            .product();
        [frequency as f32, db(gain) as f32]
    })
}

/// A compressor's output level for each input level, with `mix` of it in
/// parallel with the input.
fn compression(
    compressor: &Compressor<f32>,
    makeup_db: f64,
    mix: f64,
    points: usize,
) -> Vec<[f32; 2]> {
    linear(LEVELS_DB.0, LEVELS_DB.1, points)
        .map(|level| {
            let compressed = level - compressor.static_reduction_db(level) + makeup_db;
            let out = (1.0 - mix) * 10f64.powf(level / 20.0) + mix * 10f64.powf(compressed / 20.0);
            [level as f32, db(out) as f32]
        })
        .collect()
}

/// One cycle of the wave of shape `shape` (a parameter's value), mapped
/// through `value`.
fn cycle(shape: f64, points: usize, value: impl Fn(f64) -> f64) -> Vec<[f32; 2]> {
    let mut lfo = Lfo::new(lfo_shape(shape), 1.0);
    linear(0.0, 1.0, points)
        .map(|phase| {
            lfo.set_phase(phase.min(1.0 - 1e-12));
            [phase as f32, value(lfo.value()) as f32]
        })
        .collect()
}

/// A sine cycle mapped through `value`.
fn sine_cycle(points: usize, value: impl Fn(f64) -> f64) -> Vec<[f32; 2]> {
    linear(0.0, 1.0, points)
        .map(|phase| {
            let wave = (core::f64::consts::TAU * phase).sin();
            [phase as f32, value(wave) as f32]
        })
        .collect()
}

/// The curve of a part's `effect` at the settings in `params`; `tempo` is
/// the master's, in beats per minute. `None` for an effect a part cannot
/// have.
pub(crate) fn part_curve(
    params: &ChainParams,
    effect: Effect,
    sample_rate: f64,
    tempo: f64,
    points: usize,
) -> Option<Curve> {
    let points = points.clamp(2, MAX_CURVE_POINTS);
    let curve: Vec<[f32; 2]> = match effect {
        Effect::Lowpass | Effect::Highpass => {
            let (shape, cutoff, resonance) = if effect == Effect::Lowpass {
                let p = &params.lowpass;
                (FilterShape::Lowpass, p.cutoff.plain(), p.resonance.plain())
            } else {
                let p = &params.highpass;
                (FilterShape::Highpass, p.cutoff.plain(), p.resonance.plain())
            };
            // The state-variable filters are the bilinear transform of the
            // same prototype as the cookbook biquad, prewarped at the
            // cutoff: their responses are the same.
            let band =
                BiquadCoefficients::design(shape, sample_rate, cutoff, resonance_to_q(resonance));
            response(sample_rate, points, &[band]).collect()
        }
        Effect::Eq => {
            let p = &params.eq;
            let bands = Equaliser::coefficients(
                sample_rate,
                [p.low_gain.plain(), p.mid_gain.plain(), p.high_gain.plain()],
                [p.low_freq.plain(), p.mid_freq.plain(), p.high_freq.plain()],
                p.mid_q.plain(),
            );
            response(sample_rate, points, &bands).collect()
        }
        Effect::Drive => {
            let p = &params.drive;
            let curve = drive_curve(p.shape.plain());
            let (drive, output, mix) = (
                f64::from(gain_from_db(p.amount.plain())),
                f64::from(gain_from_db(p.output.plain())),
                p.mix.plain(),
            );
            linear(-1.0, 1.0, points)
                .map(|x| {
                    let y = x + (curve.apply(x * drive) * output - x) * mix;
                    [x as f32, y as f32]
                })
                .collect()
        }
        Effect::Bitcrush => {
            let p = &params.bitcrush;
            let step = crush_step(p.bits.plain()) as f32;
            let mix = p.mix.value();
            linear(-1.0, 1.0, points)
                .map(|x| {
                    let x = x as f32;
                    [x, x + (crush(x, step) - x) * mix]
                })
                .collect()
        }
        Effect::Compressor => {
            let p = &params.compressor;
            let mut compressor = Compressor::<f32>::new();
            crate::fx::Squeeze::configure(
                &mut compressor,
                p.threshold.plain(),
                p.ratio.plain(),
                p.knee.plain(),
                p.attack.plain(),
                p.release.plain(),
            );
            compression(&compressor, p.makeup.plain(), p.mix.plain(), points)
        }
        Effect::Chorus | Effect::Flanger => {
            let (centre, depth) = if effect == Effect::Chorus {
                (params.chorus.delay.plain(), params.chorus.depth.plain())
            } else {
                (params.flanger.delay.plain(), params.flanger.depth.plain())
            };
            // The chorus never reads closer than two frames.
            let shortest = 2_000.0 / sample_rate;
            sine_cycle(points, |wave| (centre + depth * wave).max(shortest))
        }
        Effect::Phaser => {
            let p = &params.phaser;
            let (centre, depth) = (p.center.plain(), p.depth.plain());
            let highest = 0.49 * sample_rate;
            sine_cycle(points, |wave| {
                (centre * (depth * wave).exp2()).clamp(10.0f64.min(highest), highest)
            })
        }
        Effect::Tremolo | Effect::Autopan => {
            let p = if effect == Effect::Tremolo {
                &params.tremolo
            } else {
                &params.autopan
            };
            let depth = p.depth.plain();
            cycle(p.shape.plain(), points, |wave| {
                wobble_target(effect, wave, depth)
            })
        }
        Effect::Delay => {
            let p = &params.delay;
            let seconds = if p.sync.plain() > 0.0 {
                p.sync.plain() * 60.0 / tempo
            } else {
                p.time.plain()
            };
            let seconds = (seconds * sample_rate).round().max(1.0) / sample_rate;
            delay_taps(
                seconds.min(crate::fx::DELAY_MAX_SECONDS),
                p.feedback.plain(),
                p.mix.plain(),
                p.pingpong.plain(),
                points,
            )
        }
        Effect::Reverb => {
            let p = &params.reverb;
            decay(p.decay.plain(), p.mix.plain(), points)
        }
        Effect::Glue | Effect::Limiter => return None,
    };
    Some(Curve {
        visual: effect.visual(),
        points: curve,
    })
}

/// The echoes of a short sound in the centre: time and the gain of the
/// louder side, at low frequencies (the damping only darkens them), until
/// they fall below -60 dB or `points` have been given.
fn delay_taps(
    seconds: f64,
    feedback: f64,
    mix: f64,
    pingpong: f64,
    points: usize,
) -> Vec<[f32; 2]> {
    let (straight, crossed) = (1.0 - pingpong, pingpong);
    // What goes into each line, then what each echo puts back.
    let mut sides = [1.0, straight];
    let mut taps = Vec::new();
    for echo in 1..=points {
        let loudest = sides[0].abs().max(sides[1].abs()) * mix;
        if loudest < LAST_TAP && echo > 1 {
            break;
        }
        taps.push([(echo as f64 * seconds) as f32, loudest as f32]);
        sides = [
            feedback * (straight * sides[0] + crossed * sides[1]),
            feedback * (straight * sides[1] + crossed * sides[0]),
        ];
    }
    taps
}

/// A reverb's tail: `level` (a gain) at the start, falling 60 dB in
/// `seconds`, over one and a half times that.
fn decay(seconds: f64, level: f64, points: usize) -> Vec<[f32; 2]> {
    linear(0.0, 1.5 * seconds, points)
        .map(|time| [time as f32, (db(level) - 60.0 * time / seconds) as f32])
        .collect()
}

/// The curve of the master bus's `effect`: the glue compressor, the
/// limiter, or the shared reverb. `None` for any other.
pub(crate) fn master_curve(params: &MasterParams, effect: Effect, points: usize) -> Option<Curve> {
    let points = points.clamp(2, MAX_CURVE_POINTS);
    let curve = match effect {
        Effect::Glue => {
            let p = &params.glue;
            let mut compressor = Compressor::<f32>::new();
            crate::fx::Squeeze::configure(
                &mut compressor,
                p.threshold.plain(),
                p.ratio.plain(),
                p.knee.plain(),
                p.attack.plain(),
                p.release.plain(),
            );
            compression(&compressor, p.makeup.plain(), 1.0, points)
        }
        Effect::Limiter => {
            let ceiling = params
                .limiter
                .ceiling
                .plain()
                .min(20.0 * f64::from(crate::engine::OUTPUT_CEILING).log10());
            linear(LEVELS_DB.0, LEVELS_DB.1, points)
                .map(|level| [level as f32, level.min(ceiling) as f32])
                .collect()
        }
        Effect::Reverb => {
            let p = &params.reverb;
            decay(
                p.decay.plain(),
                f64::from(gain_from_db(p.level.plain())),
                points,
            )
        }
        _ => return None,
    };
    Some(Curve {
        visual: effect.visual(),
        points: curve,
    })
}
