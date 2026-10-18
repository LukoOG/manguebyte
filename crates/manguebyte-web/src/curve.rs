//! The curves an editor draws for an effect, worked out from its
//! parameters.
//!
//! Every function here reads the parameters as they stand (the values the
//! last events set, not the smoothed values a block is processing with)
//! and computes the curve outside the render call with
//! `manguebyte-dsp`'s response curves ([`manguebyte_dsp::curve`]), which
//! come from the coefficients and limits the effects run on:
//!
//! | Effect | Curve |
//! |---|---|
//! | high-pass, low-pass | [`SvfResponse`], the state-variable filter's own |
//! | EQ | three [`BiquadResponse`]s of the bands' coefficients, in a row |
//! | drive | the [`ShaperCurve`](manguebyte_dsp::ShaperCurve)'s, or the [`tube_curve`](crate::tube_curve), between the drive's gains and its mix |
//! | compressor, glue | [`CompressorTransfer`], with the make-up and the mix |
//! | limiter | [`LimiterTransfer`] |
//! | chorus, flanger | [`ChorusSweep`] |
//! | phaser | [`PhaserSweep`] |
//! | tremolo, auto-pan | [`LfoCycle`], through what the effect does with its wave |
//! | delay | [`DelayTaps`], with the spread of a ping-pong between the sides |
//! | reverb | [`ReverbDecay`] at 1 kHz |
//!
//! The bitcrusher is this crate's own, and so is its staircase. The axes
//! are those of [`Visual`], which the crate's README lists.

use manguebyte_dsp::SvfMode;
use manguebyte_dsp::curve::{
    BiquadResponse, ChorusSweep, CompressorTransfer, Curve as _, DelayTaps, FrequencyResponse,
    LfoCycle, LimiterTransfer, PhaserSweep, ReverbDecay, SvfResponse, linear_spaced, log_spaced,
};
use manguebyte_params::Param;

use crate::effect::{Effect, Visual};
use crate::fx::{
    DriveCurve, Equaliser, crush, crush_step, drive_curve, gain_from_db, lfo_shape, resonance_to_q,
    tube_curve, wobble_target,
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

/// The frequency a reverb's tail is drawn at: where reverberation times
/// are usually quoted.
const REVERB_CURVE_HZ: f64 = 1_000.0;

/// The damping and the size of a part's reverb, which have no parameters.
const PART_REVERB_DAMPING: f64 = 0.45;
const PART_REVERB_SIZE: f64 = 0.6;

fn db(gain: f64) -> f64 {
    20.0 * gain.max(1e-12).log10()
}

/// `points` values from `from` to `to`, evenly spaced.
fn linear(from: f64, to: f64, points: usize) -> Vec<f64> {
    let mut values = vec![0.0; points];
    linear_spaced(from, to, &mut values);
    values
}

/// `points` frequencies from 20 Hz to 20 kHz or just under half the
/// sample rate, evenly spaced in octaves.
fn frequencies(sample_rate: f64, points: usize) -> Vec<f64> {
    let highest = (0.49 * sample_rate).min(20_000.0);
    let mut values = vec![0.0; points];
    log_spaced(LOWEST_HZ, highest, &mut values);
    values
}

/// The gain of `response` in decibels at each of `points` frequencies.
fn response(sample_rate: f64, points: usize, response: &impl FrequencyResponse) -> Vec<[f32; 2]> {
    frequencies(sample_rate, points)
        .into_iter()
        .map(|frequency| [frequency as f32, response.magnitude_db_at(frequency) as f32])
        .collect()
}

/// A compressor's output level for each input level, with `mix` of it in
/// parallel with the input.
fn compression(transfer: &CompressorTransfer, mix: f64, points: usize) -> Vec<[f32; 2]> {
    linear(LEVELS_DB.0, LEVELS_DB.1, points)
        .into_iter()
        .map(|level| {
            let compressed = transfer.value_at(level);
            let out = (1.0 - mix) * 10f64.powf(level / 20.0) + mix * 10f64.powf(compressed / 20.0);
            [level as f32, db(out) as f32]
        })
        .collect()
}

/// One cycle of `curve`, from its start to its end, mapped through
/// `value`.
fn cycle(
    curve: &impl manguebyte_dsp::curve::Curve,
    points: usize,
    value: impl Fn(f64) -> f64,
) -> Vec<[f32; 2]> {
    linear(0.0, 1.0, points)
        .into_iter()
        .map(|phase| {
            // The cycle repeats beyond 1: the last point is its end.
            let at = curve.value_at(phase.min(1.0 - 1e-12));
            [phase as f32, value(at) as f32]
        })
        .collect()
}

/// A reverb's tail: `level` (a gain) at the start, falling as `decay` says
/// at [`REVERB_CURVE_HZ`], over one and a half times its decay time.
fn tail(decay: &ReverbDecay, level: f64, points: usize) -> Vec<[f32; 2]> {
    let energy = decay.energy_at(REVERB_CURVE_HZ);
    linear(0.0, 1.5 * decay.decay_seconds(), points)
        .into_iter()
        .map(|time| [time as f32, (db(level) + energy.value_at(time)) as f32])
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
            // The filter runs on the parameters' values, single precision.
            let (mode, cutoff, resonance) = if effect == Effect::Lowpass {
                let p = &params.lowpass;
                (SvfMode::Lowpass, p.cutoff.value(), p.resonance.value())
            } else {
                let p = &params.highpass;
                (SvfMode::Highpass, p.cutoff.value(), p.resonance.value())
            };
            let filter = SvfResponse::new(
                mode,
                f64::from(cutoff),
                resonance_to_q(f64::from(resonance)),
                sample_rate,
            );
            response(sample_rate, points, &filter)
        }
        Effect::Eq => {
            let p = &params.eq;
            let bands = Equaliser::coefficients(
                sample_rate,
                [p.low_gain.plain(), p.mid_gain.plain(), p.high_gain.plain()],
                [p.low_freq.plain(), p.mid_freq.plain(), p.high_freq.plain()],
                p.mid_q.plain(),
            )
            .map(|band| BiquadResponse::new(band, sample_rate));
            response(sample_rate, points, &bands)
        }
        Effect::Drive => {
            let p = &params.drive;
            let curve = drive_curve(p.shape.plain());
            let (drive, output, mix) = (
                f64::from(gain_from_db(p.amount.plain())),
                f64::from(gain_from_db(p.output.plain())),
                p.mix.plain(),
            );
            // The tube's curve as it bends; the offset its drive takes out
            // after it is not drawn.
            let bend = |x: f64| match curve {
                DriveCurve::Shaper(curve) => curve.value_at(x),
                DriveCurve::Tube => tube_curve(x),
            };
            linear(-1.0, 1.0, points)
                .into_iter()
                .map(|x| {
                    let y = x + (bend(x * drive) * output - x) * mix;
                    [x as f32, y as f32]
                })
                .collect()
        }
        Effect::Bitcrush => {
            let p = &params.bitcrush;
            let step = crush_step(p.bits.plain()) as f32;
            let mix = p.mix.value();
            linear(-1.0, 1.0, points)
                .into_iter()
                .map(|x| {
                    let x = x as f32;
                    [x, x + (crush(x, step) - x) * mix]
                })
                .collect()
        }
        Effect::Compressor => {
            let p = &params.compressor;
            let transfer = CompressorTransfer::new(
                p.threshold.plain(),
                p.ratio.plain(),
                p.knee.plain(),
                p.makeup.plain(),
            );
            compression(&transfer, p.mix.plain(), points)
        }
        Effect::Chorus | Effect::Flanger => {
            let (centre, depth, rate) = if effect == Effect::Chorus {
                let p = &params.chorus;
                (p.delay.plain(), p.depth.plain(), p.rate.plain())
            } else {
                let p = &params.flanger;
                (p.delay.plain(), p.depth.plain(), p.rate.plain())
            };
            cycle(
                &ChorusSweep::new(centre, depth, rate, sample_rate),
                points,
                |ms| ms,
            )
        }
        Effect::Phaser => {
            let p = &params.phaser;
            let sweep = PhaserSweep::new(
                p.center.plain(),
                p.depth.plain(),
                p.rate.plain(),
                sample_rate,
            );
            cycle(&sweep, points, |hertz| hertz)
        }
        Effect::Tremolo | Effect::Autopan => {
            let p = if effect == Effect::Tremolo {
                &params.tremolo
            } else {
                &params.autopan
            };
            let depth = p.depth.plain();
            let wave = LfoCycle::new(lfo_shape(p.shape.plain()));
            cycle(&wave, points, |wave| wobble_target(effect, wave, depth))
        }
        Effect::Delay => {
            let p = &params.delay;
            let seconds = if p.sync.plain() > 0.0 {
                p.sync.plain() * 60.0 / tempo
            } else {
                p.time.plain()
            };
            // The delay reads its lines a whole number of frames back.
            let seconds = (seconds * sample_rate).round().max(1.0) / sample_rate;
            let taps = DelayTaps::new(
                seconds.min(crate::fx::DELAY_MAX_SECONDS),
                p.feedback.plain(),
                p.mix.plain(),
                sample_rate,
            );
            delay_taps(&taps, p.pingpong.plain(), points)
        }
        Effect::Reverb => {
            let p = &params.reverb;
            let decay = ReverbDecay::new(
                p.decay.plain(),
                PART_REVERB_DAMPING,
                PART_REVERB_SIZE,
                sample_rate,
            );
            tail(&decay, p.mix.plain(), points)
        }
        Effect::Glue | Effect::Limiter => return None,
    };
    Some(Curve {
        visual: effect.visual(),
        points: curve,
    })
}

/// The echoes of a short sound in the centre, from the first: their times
/// and the gain of the louder side, at low frequencies (the damping only
/// darkens them), until they fall below -60 dB or `points` have been
/// given.
///
/// A delay that does not ping-pong echoes each side on itself, the echoes
/// of [`DelayTaps`]. The ping-pong sends the sound to the left line and
/// the right side's share (`1 - pingpong`) to the right one, and crosses
/// `pingpong` of the feedback over: the sum of the two sides' echoes keeps
/// the taps' gains, and their difference falls by `|1 - 2·pingpong|` an
/// echo more. The louder side of echo `k` is therefore the tap's gain
/// times `1 - p/2 + p/2·|1 - 2p|^(k-1)`, with `p` the ping-pong: 1 at
/// either end, every echo on one side or the other.
fn delay_taps(taps: &DelayTaps, pingpong: f64, points: usize) -> Vec<[f32; 2]> {
    let p = pingpong.clamp(0.0, 1.0);
    let (common, apart, narrowing) = (1.0 - 0.5 * p, 0.5 * p, (1.0 - 2.0 * p).abs());
    let mut spread = 1.0;
    let mut echoes = Vec::new();
    for echo in 1..=points {
        let tap = taps.tap(echo);
        let louder = tap.y * (common + apart * spread);
        if louder < LAST_TAP && echo > 1 {
            break;
        }
        echoes.push([tap.x as f32, louder as f32]);
        spread *= narrowing;
    }
    echoes
}

/// The curve of the master bus's `effect`: the glue compressor, the
/// limiter, or the shared reverb. `None` for any other.
pub(crate) fn master_curve(
    params: &MasterParams,
    effect: Effect,
    sample_rate: f64,
    points: usize,
) -> Option<Curve> {
    let points = points.clamp(2, MAX_CURVE_POINTS);
    let curve = match effect {
        Effect::Glue => {
            let p = &params.glue;
            let transfer = CompressorTransfer::new(
                p.threshold.plain(),
                p.ratio.plain(),
                p.knee.plain(),
                p.makeup.plain(),
            );
            compression(&transfer, 1.0, points)
        }
        Effect::Limiter => {
            // No sample ever goes beyond the output ceiling, whatever the
            // limiter's own.
            let ceiling = params
                .limiter
                .ceiling
                .plain()
                .min(20.0 * f64::from(crate::engine::OUTPUT_CEILING).log10());
            let transfer = LimiterTransfer::new(ceiling);
            linear(LEVELS_DB.0, LEVELS_DB.1, points)
                .into_iter()
                .map(|level| [level as f32, transfer.value_at(level) as f32])
                .collect()
        }
        Effect::Reverb => {
            let p = &params.reverb;
            let decay = ReverbDecay::new(
                p.decay.plain(),
                p.damping.plain(),
                p.size.plain(),
                sample_rate,
            );
            tail(&decay, f64::from(gain_from_db(p.level.plain())), points)
        }
        _ => return None,
    };
    Some(Curve {
        visual: effect.visual(),
        points: curve,
    })
}
