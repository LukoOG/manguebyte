use core::marker::PhantomData;

use manguebyte_core::{AudioBuffer, Sample, Smoothed};

use crate::curve::LadderResponse;
use crate::cutoff::GlidingCutoff;
use crate::numeric::{glide_to, limit};
use crate::{ProcessSpec, Processor};

/// Limits of the cutoff: a lowest frequency in hertz, and a highest one as
/// a fraction of the sample rate.
const LOWEST_HZ: f64 = 5.0;
const HIGHEST_RATIO: f64 = 0.45;

/// Limits of the drive, in decibels.
const MIN_DRIVE_DB: f64 = -24.0;
const MAX_DRIVE_DB: f64 = 48.0;

/// Feedback around the four stages at which the ladder starts to oscillate
/// by itself: the gain that brings two of its poles onto the imaginary axis.
const SELF_OSCILLATION: f64 = 4.0;

/// How long the cutoff takes to reach a new value when a block is
/// processed.
const CUTOFF_GLIDE_SECONDS: f64 = 0.01;
/// How long a change of resonance, drive or mode takes.
const GAIN_GLIDE_SECONDS: f64 = 0.02;

/// Most steps of Newton's method spent on one sample.
const MAX_ITERATIONS: usize = 12;
/// A step this small, against the value it corrects, is the last one: the
/// error left after it is its square.
const CONVERGED: f64 = 1e-9;
/// Largest input to the saturator that is told apart from a larger one.
const LARGEST_ARGUMENT: f64 = 1e30;

/// The integrator gain a filter set to `frequency` arrives at, at
/// `sample_rate`: the same limits and the same arithmetic as the filter.
pub(crate) fn integrator_gain(frequency: f64, sample_rate: f64) -> f64 {
    let mut cutoff = GlidingCutoff::new(frequency, LOWEST_HZ, HIGHEST_RATIO, CUTOFF_GLIDE_SECONDS);
    cutoff.prepare(sample_rate);
    cutoff.gain()
}

/// The gain of the feedback around the four stages for a resonance that
/// is already limited.
pub(crate) fn feedback(resonance: f64) -> f64 {
    SELF_OSCILLATION * resonance
}

/// A drive in decibels as the filter keeps it: limited to `-24.0..=48.0`,
/// with not-a-number taken as 0 dB.
pub(crate) fn limited_drive_db(db: f64) -> f64 {
    if db.is_nan() {
        0.0
    } else {
        limit(db, MIN_DRIVE_DB, MAX_DRIVE_DB)
    }
}

/// The plain gain of a drive in decibels.
pub(crate) fn drive_gain(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// Which response a [`LadderFilter`] produces.
///
/// All six are taken from the same four stages, by adding their outputs
/// with different weights, so the resonance is the same in every mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LadderMode {
    /// Passes what is below the cutoff, 12 dB per octave.
    Lp12,
    /// Passes what is below the cutoff, 24 dB per octave. The classic
    /// response of the transistor ladder.
    Lp24,
    /// Passes what is above the cutoff, 12 dB per octave.
    Hp12,
    /// Passes what is above the cutoff, 24 dB per octave.
    Hp24,
    /// Passes a band around the cutoff, 6 dB per octave on either side.
    Bp12,
    /// Passes a band around the cutoff, 12 dB per octave on either side.
    Bp24,
}

impl LadderMode {
    /// Every mode.
    pub const ALL: [Self; 6] = [
        Self::Lp12,
        Self::Lp24,
        Self::Hp12,
        Self::Hp24,
        Self::Bp12,
        Self::Bp24,
    ];

    /// How much of the input of the first stage and of the output of each
    /// of the four stages the mode takes.
    ///
    /// With `L = 1 / (1 + s)` for one stage, the output of stage `n` is
    /// `L^n` times the input of the first, and a first-order high-pass is
    /// `1 - L`. The weights are the coefficients of `L²`, `L⁴`, `(1 - L)²`,
    /// `(1 - L)⁴`, `2 L (1 - L)` and `4 L² (1 - L)²`; the factors 2 and 4
    /// give the band-pass modes unity gain at the cutoff without resonance.
    fn taps(self) -> [f64; 5] {
        match self {
            Self::Lp12 => [0.0, 0.0, 1.0, 0.0, 0.0],
            Self::Lp24 => [0.0, 0.0, 0.0, 0.0, 1.0],
            Self::Hp12 => [1.0, -2.0, 1.0, 0.0, 0.0],
            Self::Hp24 => [1.0, -4.0, 6.0, -4.0, 1.0],
            Self::Bp12 => [0.0, 2.0, -2.0, 0.0, 0.0],
            Self::Bp24 => [0.0, 0.0, 4.0, -8.0, 4.0],
        }
    }

    /// The largest output the mode can produce while the cutoff stays at or
    /// below a quarter of the sample rate (counted from the last
    /// [`prepare`](Processor::prepare) or [`reset`](Processor::reset)),
    /// whatever the input and however the cutoff, resonance and drive move:
    /// 1 for the low-pass modes, 4 for [`Hp12`](Self::Hp12) and
    /// [`Bp12`](Self::Bp12), 16 for [`Hp24`](Self::Hp24) and
    /// [`Bp24`](Self::Bp24).
    ///
    /// Up to that cutoff no stage can hold or put out more than the
    /// saturator lets into the ladder, which is 1, so the output cannot
    /// exceed the sum of the sizes of the weights the mode gives the stages.
    ///
    /// That is the bound of a mode in force. A change of mode glides, over
    /// 20 ms when a block is processed, from the old mode's weights to the
    /// new one's in straight lines, and on the way the output is bounded by
    /// the larger of the two modes' bounds instead: switched from
    /// [`Hp12`](Self::Hp12) to [`Lp24`](Self::Lp24), 4 until the glide has
    /// arrived and 1 after, although [`LadderFilter::mode`] reports `Lp24`
    /// at once. (If the mode changes again before arriving, the largest
    /// bound of the modes it glided between.) See [`LadderFilter`] for what
    /// happens above a quarter of the sample rate.
    pub fn output_bound(self) -> f64 {
        self.taps().iter().map(|tap| tap.abs()).sum()
    }
}

/// Everything that moves while a block is processed. Every channel runs a
/// copy of its own, so all of them see the same values.
#[derive(Debug, Clone, Copy)]
struct Motion {
    cutoff: GlidingCutoff,
    /// `g / (1 + g)` for the integrator gain `g` of the cutoff.
    gain: f64,
    /// Gain of the feedback around the four stages, 0 to 4.
    feedback: Smoothed<f64>,
    /// Gain in front of the saturator.
    drive: Smoothed<f64>,
    /// The weights of the mode.
    taps: [Smoothed<f64>; 5],
}

/// The values of [`Motion`] for one sample.
#[derive(Debug, Clone, Copy)]
struct Step {
    gain: f64,
    feedback: f64,
    drive: f64,
    taps: [f64; 5],
}

impl Motion {
    /// Moves on by one sample and returns the values for it.
    #[inline]
    fn step(&mut self) -> Step {
        if self.cutoff.is_gliding() {
            let g = self.cutoff.next_gain();
            self.gain = g / (1.0 + g);
        }
        let mut taps = [0.0; 5];
        for (tap, moving) in taps.iter_mut().zip(&mut self.taps) {
            *tap = moving.next_value();
        }
        Step {
            gain: self.gain,
            feedback: self.feedback.next_value(),
            drive: self.drive.next_value(),
            taps,
        }
    }

    /// The values everything is heading for.
    fn settled(&self) -> Step {
        let mut taps = [0.0; 5];
        for (tap, moving) in taps.iter_mut().zip(&self.taps) {
            *tap = moving.target();
        }
        Step {
            gain: self.gain,
            feedback: self.feedback.target(),
            drive: self.drive.target(),
            taps,
        }
    }

    fn is_moving(&self) -> bool {
        self.cutoff.is_gliding()
            || self.feedback.is_smoothing()
            || self.drive.is_smoothing()
            || self.taps.iter().any(Smoothed::is_smoothing)
    }

    /// Jumps to where everything is heading.
    fn snap(&mut self) {
        self.cutoff.snap();
        let g = self.cutoff.gain();
        self.gain = g / (1.0 + g);
        self.feedback.set_immediate(self.feedback.target());
        self.drive.set_immediate(self.drive.target());
        for tap in &mut self.taps {
            tap.set_immediate(tap.target());
        }
    }
}

/// Solves `v + b·tanh(v) = a` for `v` and returns `tanh(v)`, for `b` of
/// zero or more.
///
/// This is the feedback loop of the ladder for one sample: `tanh(v)` is
/// what the saturator puts out when the loop around it has no delay. The
/// left side grows with `v` at a rate between 1 and `1 + b`, so there is
/// exactly one solution. For `a` above zero it lies above zero, where the
/// left side is concave, and the solution of the same equation without the
/// saturator, `a / (1 + b)`, lies below it. Newton's method started there
/// therefore climbs to the solution without ever passing it, and whenever
/// it is stopped the result is a value the saturator can produce: between
/// -1 and 1. A negative `a` is the mirror image. An `a` that is not a
/// number counts as zero.
#[inline]
pub(crate) fn saturated(a: f64, b: f64) -> f64 {
    solve_loop(a, b).0
}

/// [`saturated`], and the number of steps it took.
#[inline]
fn solve_loop(a: f64, b: f64) -> (f64, usize) {
    let target = limit(a.abs(), 0.0, LARGEST_ARGUMENT);
    let mut v = target / (1.0 + b);
    let mut tanh = v.tanh();
    let mut steps = 0;
    while steps < MAX_ITERATIONS {
        let step = (v + b * tanh - target) / (1.0 + b * (1.0 - tanh * tanh));
        v -= step;
        tanh = v.tanh();
        steps += 1;
        if step.abs() <= CONVERGED * v {
            break;
        }
    }
    (tanh.copysign(a), steps)
}

/// One sample through the ladder.
///
/// Why the ladder cannot run away, whatever the cutoff does: each stage
/// computes `y = G·x + (1 - G)·s` and keeps `s' = 2·y - s`, where `G = g /
/// (1 + g)` and `g = tan(π·fc/fs)`, so that `s' = 2G·x + (1 - 2G)·s`. Let
/// `|x| <= X` and `|s| <= h·X` with `h = max(1, g_max)`. For `G <= 1/2`
/// (`g <= 1`) the new state is a mixture of `x` and `s`, so `|s'| <= h·X`.
/// Above that `|s'| <= (2G + (2G - 1)·h)·X`, which is at most `h·X` exactly
/// when `g <= h`. The bound therefore holds for ever, and `y`, which lies
/// between `x` and `s`, obeys it too. The saturator feeds the first stage at
/// most 1, so stage `n` puts out at most `h^n`: 1 while the cutoff stays at
/// or below a quarter of the sample rate, `tan(0.45·π)^n` in any case.
#[inline]
fn tick(state: &mut [f64; 4], x: f64, step: &Step) -> f64 {
    let gain = step.gain;
    let rest = 1.0 - gain;
    let gain2 = gain * gain;
    // What the fourth stage would put out for a silent input of the first:
    // the part of its output that is already decided by the memories.
    let decided = rest * (gain2 * gain * state[0] + gain2 * state[1] + gain * state[2] + state[3]);
    // The fourth stage puts out `gain⁴ u + decided` for an input `u` of the
    // first, and `u = tanh(drive x - feedback (gain⁴ u + decided))`.
    let u = saturated(
        step.drive * x - step.feedback * decided,
        step.feedback * gain2 * gain2,
    );

    let mut output = step.taps[0] * u;
    let mut signal = u;
    for (memory, tap) in state.iter_mut().zip(&step.taps[1..]) {
        let v = (signal - *memory) * gain;
        signal = v + *memory;
        *memory = signal + v;
        output += tap * signal;
    }
    output
}

/// A four-pole transistor-ladder filter: the resonant low-pass of the
/// classic analogue synthesiser, with high-pass and band-pass responses
/// taken from the same four stages.
///
/// The model is the one in Vadim Zavalishin, "The Art of VA Filter Design",
/// chapters 4 to 6: four one-pole low-pass stages in a row, each a
/// trapezoidal integrator, with the output of the last one fed back to the
/// input of the first through a saturator.
///
/// ```text
///   x --(drive)--(+)--[tanh]--u--[1 pole]--[1 pole]--[1 pole]--[1 pole]--+-- y4
///                 ^ -                                                    |
///                 +---------------------(4 x resonance)------------------+
/// ```
///
/// The loop has no delay in it. For every sample the equation
/// `u = tanh(drive·x - 4·resonance·y4(u))` is solved for `u` by Newton's
/// method, which here converges from one side and so cannot overshoot;
/// measured, it takes 1 step for a quiet signal and at most 5 for any level
/// and resonance, and it is stopped after 12 whatever happens. A ladder
/// whose loop is closed through a one-sample delay instead goes out of tune
/// and changes its resonance as the cutoff rises; this one does neither.
///
/// # Response
///
/// For signals well below the level of the saturator the filter is linear.
/// With `s = j·tan(π·f/fs) / tan(π·fc/fs)` and `k = 4·resonance`, its gain
/// is the drive times
///
/// ```text
/// Lp24:  1 / ((1 + s)⁴ + k)              Lp12:  (1 + s)² / ((1 + s)⁴ + k)
/// Hp24:  s⁴ / ((1 + s)⁴ + k)             Hp12:  s² (1 + s)² / ((1 + s)⁴ + k)
/// Bp24:  4 s² / ((1 + s)⁴ + k)           Bp12:  2 s (1 + s)² / ((1 + s)⁴ + k)
/// ```
///
/// Without resonance each stage is 3.01 dB down at the cutoff, so
/// [`Lp24`](LadderMode::Lp24) and [`Hp24`](LadderMode::Hp24) are 12.04 dB
/// down there, [`Lp12`](LadderMode::Lp12) and [`Hp12`](LadderMode::Hp12)
/// 6.02 dB, and the band-pass modes are at unity.
///
/// The resonance runs from 0 to 1. It raises a peak at `resonance^(1/4)`
/// times the cutoff (0.974 of it at 0.9, 0.997 at 0.99), and at 1 two poles
/// reach the unit circle exactly at the cutoff: the edge of
/// self-oscillation, where a click rings on without dying away. As in the
/// circuit, the feedback is taken off the input, so resonance lowers what
/// is below the cutoff: the gain of the low-pass modes at 0 Hz is
/// `1 / (1 + 4·resonance)`, 14 dB down at full resonance. The high-pass
/// modes keep unity gain far above the cutoff.
///
/// # Drive
///
/// The drive is a gain in front of the saturator, from -24 to +48 dB. As
/// with [`Waveshaper`](crate::Waveshaper), quiet signals come out louder by
/// the drive and loud ones are held back: the saturator is a `tanh`, the
/// curve of a transistor pair, and what leaves it is never larger than 1.
/// At 0 dB a full-scale signal is already compressed by its peaks
/// (`tanh(1) = 0.76`); use a negative drive and a [`Gain`](crate::Gain)
/// after the filter for a cleaner one.
///
/// # Use
///
/// - As a [`Processor`], on blocks. A new cutoff is approached over 10 ms
///   on a musical scale, and resonance, drive and mode over 20 ms, so
///   settings changed once per block neither click nor step.
/// - By the sample, with [`process_sample`](Self::process_sample), which
///   applies the settings at once. Call
///   [`set_frequency`](Self::set_frequency) before each sample to modulate
///   the cutoff at audio rate.
///
/// ```
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_dsp::{LadderFilter, LadderMode, ProcessSpec, Processor};
///
/// let mut filter = LadderFilter::<f32>::new(LadderMode::Lp24, 1_000.0).with_resonance(0.5);
/// filter.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 4_800, num_channels: 1 });
///
/// // A quiet steady level settles at 1 / (1 + 4 x 0.5) of itself.
/// let mut block = AudioBuffer::<f32>::new(1, 4_800);
/// block.channel_mut(0).fill(0.001);
/// filter.process(&mut block);
/// assert!((block.channel(0)[4_799] - 0.001 / 3.0).abs() < 1e-7);
/// ```
///
/// # Limits
///
/// The cutoff is limited to between 5 Hz and 0.45 of the sample rate, for
/// the reason given below, so the filter cannot be opened all the way: at
/// 44.1 kHz, fully open (at 19.8 kHz) and without resonance,
/// [`Lp24`](LadderMode::Lp24) is 1.4 dB down at 15 kHz and 4.4 dB down at
/// 18 kHz. Where a transparent top end matters, take the filter out of the
/// signal path rather than opening it.
///
/// The filter cannot run away. Whatever arrives, numbers that are not
/// finite included, the saturator lets no more than 1 into the ladder, and
/// the stages are stable for every cutoff. How large the output can get
/// depends on where the cutoff is:
///
/// - As long as it has stayed at or below a quarter of the sample rate
///   since the filter was prepared or reset, no stage can hold or put out
///   more than 1, however the settings move. The output is then at most
///   [`LadderMode::output_bound`] of the mode in force: 1 for the low-pass
///   modes, 4 for `Hp12` and `Bp12`, 16 for `Hp24` and `Bp24`; while a
///   change of mode glides, the larger of the old and the new mode's.
/// - Above that, a trapezoidal integrator can hold more than its input, up
///   to `tan(π·fc/fs)` times as much for an input at half the sample rate,
///   which is 6.31 at the upper limit and the reason that limit is not
///   higher. A filter that is held still cancels this in its output; a
///   cutoff that moves can let it out. Stage `n` can then never put out
///   more than `6.31^n`, which bounds every mode (`Lp24` by 1589, `Hp24` by
///   2861). That is a proof that the filter cannot run away, not a level to
///   expect: the largest output a randomised search could provoke, with
///   full resonance and drive, a square wave at half the sample rate and
///   the cutoff thrown between its extremes, was 4.7 times the bound of the
///   first case, and noise with the cutoff set at random on every sample
///   reached 3.2 times it. Those two figures are measurements, not limits.
///
/// One sample costs two to seven evaluations of `tanh`. The saturator
/// makes harmonics, and those above half the sample rate fold back; wrap
/// the filter in [`Oversampled`](crate::Oversampled) where that matters.
/// Nothing compensates for the loss of level below the cutoff as the
/// resonance rises.
#[derive(Debug, Clone)]
pub struct LadderFilter<S: Sample = f32> {
    mode: LadderMode,
    resonance: f64,
    drive_db: f64,
    motion: Motion,
    /// The memory of the four stages of each channel.
    state: Vec<[f64; 4]>,
    marker: PhantomData<S>,
}

impl<S: Sample> LadderFilter<S> {
    /// Creates a filter with no resonance and no drive. Until
    /// [`prepare`](Processor::prepare) is called it assumes 44.1 kHz.
    pub fn new(mode: LadderMode, frequency: f64) -> Self {
        let mut motion = Motion {
            cutoff: GlidingCutoff::new(frequency, LOWEST_HZ, HIGHEST_RATIO, CUTOFF_GLIDE_SECONDS),
            gain: 0.0,
            feedback: Smoothed::new(0.0),
            drive: Smoothed::new(1.0),
            taps: mode.taps().map(Smoothed::new),
        };
        motion.snap();
        Self {
            mode,
            resonance: 0.0,
            drive_db: 0.0,
            motion,
            state: Vec::new(),
            marker: PhantomData,
        }
    }

    /// Changes the response. Real-time safe.
    pub fn set_mode(&mut self, mode: LadderMode) {
        self.mode = mode;
        for (tap, target) in self.motion.taps.iter_mut().zip(mode.taps()) {
            glide_to(tap, target);
        }
    }

    /// Changes the cutoff in hertz, limited to between 5 Hz and 0.45 of the
    /// sample rate. Real-time safe, and meant to be called as often as for
    /// every sample.
    pub fn set_frequency(&mut self, frequency: f64) {
        self.motion.cutoff.set(frequency);
        if !self.motion.cutoff.is_gliding() {
            let g = self.motion.cutoff.gain();
            self.motion.gain = g / (1.0 + g);
        }
    }

    /// Sets the resonance, limited to `0.0..=1.0`: none at 0, the edge of
    /// self-oscillation at 1. Real-time safe.
    pub fn set_resonance(&mut self, resonance: f64) {
        self.resonance = limit(resonance, 0.0, 1.0);
        glide_to(&mut self.motion.feedback, feedback(self.resonance));
    }

    /// Sets the gain in front of the saturator, in decibels, limited to
    /// `-24.0..=48.0`. Real-time safe.
    pub fn set_drive_db(&mut self, db: f64) {
        self.drive_db = limited_drive_db(db);
        glide_to(&mut self.motion.drive, drive_gain(self.drive_db));
    }

    /// Builder form of [`set_resonance`](Self::set_resonance).
    #[must_use]
    pub fn with_resonance(mut self, resonance: f64) -> Self {
        self.set_resonance(resonance);
        self
    }

    /// Builder form of [`set_drive_db`](Self::set_drive_db).
    #[must_use]
    pub fn with_drive_db(mut self, db: f64) -> Self {
        self.set_drive_db(db);
        self
    }

    /// The response, as last set.
    pub fn mode(&self) -> LadderMode {
        self.mode
    }

    /// The cutoff in hertz the filter is at or heading for, after the
    /// limits.
    pub fn frequency(&self) -> f64 {
        self.motion.cutoff.limited()
    }

    /// The resonance, as last set and limited.
    pub fn resonance(&self) -> f64 {
        self.resonance
    }

    /// The drive in decibels, as last set and limited.
    pub fn drive_db(&self) -> f64 {
        self.drive_db
    }

    /// The frequency response of the linear model at the settings the
    /// filter is at or heading for, for drawing: what a signal well below
    /// the level of the saturator gets. See [`LadderResponse`] and
    /// [`curve`](crate::curve).
    pub fn response(&self) -> LadderResponse {
        let cutoff = &self.motion.cutoff;
        LadderResponse::from_parts(
            self.mode,
            cutoff.target_gain(),
            feedback(self.resonance),
            drive_gain(self.drive_db),
            cutoff.sample_rate(),
        )
    }

    /// Filters one sample of one channel with the settings as they stand,
    /// without gliding. Real-time safe.
    ///
    /// Use this, with [`set_frequency`](Self::set_frequency) before each
    /// call, to modulate the cutoff at audio rate. A channel the filter was
    /// not prepared for is passed through untouched.
    #[inline]
    pub fn process_sample(&mut self, channel: usize, input: S) -> S {
        if self.motion.is_moving() {
            self.motion.snap();
        }
        let Some(state) = self.state.get_mut(channel) else {
            return input;
        };
        S::from_f64(tick(state, input.to_f64(), &self.motion.settled()))
    }
}

impl<S: Sample> Processor<S> for LadderFilter<S> {
    fn prepare(&mut self, spec: &ProcessSpec) {
        let motion = &mut self.motion;
        motion.cutoff.prepare(spec.sample_rate);
        motion
            .feedback
            .set_ramp(spec.sample_rate, GAIN_GLIDE_SECONDS);
        motion.drive.set_ramp(spec.sample_rate, GAIN_GLIDE_SECONDS);
        for tap in &mut motion.taps {
            tap.set_ramp(spec.sample_rate, GAIN_GLIDE_SECONDS);
        }
        motion.snap();
        self.state = vec![[0.0; 4]; spec.num_channels];
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        let mut after = self.motion;
        let mut ran = false;
        for (samples, state) in buffer.iter_channels_mut().zip(&mut self.state) {
            let mut motion = self.motion;
            let mut memory = *state;
            for sample in samples {
                let step = motion.step();
                *sample = S::from_f64(tick(&mut memory, sample.to_f64(), &step));
            }
            *state = memory;
            after = motion;
            ran = true;
        }
        if !ran {
            // No channel to process, but time still passes.
            for _ in 0..buffer.num_frames() {
                after.step();
            }
        }
        self.motion = after;
    }

    fn reset(&mut self) {
        self.state.fill([0.0; 4]);
        self.motion.snap();
    }
}

#[cfg(test)]
mod tests {
    use core::f64::consts::PI;

    use super::*;
    use crate::fft::Complex;
    use crate::numeric::{strict_buffer_peak, strict_peak};
    use crate::testing::{
        Random, db, gain_at, largest_jump, noise, run_in_blocks, sine, tail_peak,
    };

    const RATE: f64 = 48_000.0;

    fn spec(sample_rate: f64, channels: usize) -> ProcessSpec {
        ProcessSpec {
            sample_rate,
            max_block_size: 512,
            num_channels: channels,
        }
    }

    fn prepared(filter: LadderFilter<f64>, sample_rate: f64) -> LadderFilter<f64> {
        let mut filter = filter;
        filter.prepare(&spec(sample_rate, 1));
        filter
    }

    /// The response to an impulse so quiet that the saturator is a wire,
    /// scaled to that of an impulse of 1, and checked to have died away.
    fn impulse_response(filter: &mut LadderFilter<f64>, len: usize) -> Vec<f64> {
        const LEVEL: f64 = 1e-6;
        let mut impulse = AudioBuffer::<f64>::new(1, len);
        impulse.channel_mut(0)[0] = LEVEL;
        run_in_blocks(filter, &mut impulse, 512);
        let response: Vec<f64> = impulse.channel(0).iter().map(|s| s / LEVEL).collect();
        assert!(tail_peak(&response, 64) < 1e-12, "the response is cut off");
        response
    }

    /// The gain the book gives the linear ladder, at the frequency the
    /// bilinear transform maps `frequency` to.
    fn theory(mode: LadderMode, cutoff: f64, resonance: f64, rate: f64, frequency: f64) -> f64 {
        let s = Complex::new(
            0.0,
            (PI * frequency / rate).tan() / (PI * cutoff / rate).tan(),
        );
        let one = Complex::new(1.0, 0.0);
        let stage = one + s;
        let stage2 = stage * stage;
        let loop_ = stage2 * stage2 + Complex::new(4.0 * resonance, 0.0);
        let numerator = match mode {
            LadderMode::Lp24 => one,
            LadderMode::Lp12 => stage2,
            LadderMode::Hp24 => s * s * s * s,
            LadderMode::Hp12 => s * s * stage2,
            LadderMode::Bp24 => (s * s).scale(4.0),
            LadderMode::Bp12 => (s * stage2).scale(2.0),
        };
        numerator.norm() / loop_.norm()
    }

    #[test]
    fn the_loop_is_solved_to_the_last_digits_within_a_few_steps() {
        let mut worst = 0.0f64;
        let mut most_steps = 0;
        for exponent in -80..=80 {
            let a = 10f64.powf(f64::from(exponent) / 16.0);
            for b in [0.0, 1e-9, 0.01, 0.3, 1.0, 2.5, 4.0] {
                for sign in [1.0, -1.0] {
                    let (u, steps) = solve_loop(sign * a, b);
                    assert_eq!(u, saturated(sign * a, b));
                    most_steps = most_steps.max(steps);
                    // A quiet signal needs one step, which changes nothing.
                    if a < 1e-4 {
                        assert_eq!(steps, 1, "{a} with {b}");
                    }
                    assert!(u.abs() <= 1.0);
                    assert_eq!(u.signum(), sign);
                    // Put back into the equation: v = atanh(u), unless u is
                    // so close to 1 that v can no longer be read from it.
                    if u.abs() < 0.999 {
                        let v = u.abs().atanh();
                        let residual = (v + b * u.abs() - a).abs() / a;
                        worst = worst.max(residual);
                    } else {
                        // There the equation is v = a - b to within the
                        // last digits, and tanh of that is what came out.
                        assert!((u.abs() - (a - b * u.abs()).tanh()).abs() < 1e-15);
                    }
                }
            }
        }
        assert!(worst < 1e-13, "the worst residual was {worst}");
        // The most steps any of these took. The documentation states the 5
        // measured here; one more is allowed for another maths library,
        // whose `tanh` may round the last digit differently.
        assert!((5..=6).contains(&most_steps), "{most_steps} steps at most");

        // Without the saturator in play the loop is the linear one.
        assert_eq!(saturated(0.0, 4.0), 0.0);
        let quiet = saturated(1e-9, 4.0);
        assert!((quiet - 2e-10).abs() < 1e-24);
        // Garbage is not passed on.
        assert_eq!(saturated(f64::NAN, 4.0), 0.0);
        assert_eq!(saturated(f64::INFINITY, 4.0), 1.0);
        assert_eq!(saturated(f64::NEG_INFINITY, 0.0), -1.0);
        assert_eq!(saturated(1e300, 4.0), 1.0);
    }

    #[test]
    fn the_quiet_response_is_that_of_the_ladder_in_every_mode() {
        const LEN: usize = 1 << 15;
        let mut worst = 0.0f64;
        for mode in LadderMode::ALL {
            for cutoff in [300.0, 4_000.0] {
                for resonance in [0.0, 0.5, 0.9] {
                    let filter = LadderFilter::new(mode, cutoff).with_resonance(resonance);
                    let response = impulse_response(&mut prepared(filter, RATE), LEN);
                    for frequency in [0.0, 20.0, 150.0, 290.0, 300.0, 1_000.0, 3_900.0, 9_000.0] {
                        let expected = theory(mode, cutoff, resonance, RATE, frequency);
                        let measured = gain_at(&response, frequency, RATE);
                        let error = (measured - expected).abs();
                        worst = worst.max(error);
                        assert!(
                            error < 1e-9,
                            "{mode:?} at {cutoff} Hz, resonance {resonance}, {frequency} Hz: \
                             {measured} against {expected}"
                        );
                    }
                }
            }
        }
        assert!(worst < 1e-9, "worst {worst}");

        // The facts that follow from it. Without resonance, at the cutoff:
        let at_cutoff = |mode: LadderMode| {
            let filter = LadderFilter::new(mode, 1_000.0);
            db(gain_at(
                &impulse_response(&mut prepared(filter, RATE), LEN),
                1_000.0,
                RATE,
            ))
        };
        assert!((at_cutoff(LadderMode::Lp24) + 12.0412).abs() < 1e-4);
        assert!((at_cutoff(LadderMode::Hp24) + 12.0412).abs() < 1e-4);
        assert!((at_cutoff(LadderMode::Lp12) + 6.0206).abs() < 1e-4);
        assert!((at_cutoff(LadderMode::Hp12) + 6.0206).abs() < 1e-4);
        assert!(at_cutoff(LadderMode::Bp12).abs() < 1e-4);
        assert!(at_cutoff(LadderMode::Bp24).abs() < 1e-4);
        // And resonance takes level away below the cutoff: 1 / (1 + 4r).
        for mode in [LadderMode::Lp12, LadderMode::Lp24] {
            for resonance in [0.0, 0.25, 0.75] {
                let filter = LadderFilter::new(mode, 1_000.0).with_resonance(resonance);
                let mut filter = prepared(filter, RATE);
                let mut level = AudioBuffer::<f64>::new(1, 48_000);
                level.channel_mut(0).fill(1e-6);
                run_in_blocks(&mut filter, &mut level, 512);
                let expected = 1e-6 / (1.0 + 4.0 * resonance);
                assert!((level.channel(0)[47_999] - expected).abs() < 1e-15);
            }
        }
    }

    #[test]
    fn the_slopes_are_twelve_and_twenty_four_decibels_per_octave() {
        const RATE: f64 = 96_000.0;
        const LEN: usize = 1 << 15;
        // Far above a cutoff of 50 Hz, and far below one of 8 kHz.
        let slope = |mode: LadderMode, cutoff: f64, low: f64| {
            let response =
                impulse_response(&mut prepared(LadderFilter::new(mode, cutoff), RATE), LEN);
            db(gain_at(&response, 2.0 * low, RATE)) - db(gain_at(&response, low, RATE))
        };
        let above = |mode| slope(mode, 50.0, 800.0);
        let below = |mode| slope(mode, 8_000.0, 125.0);

        for (measured, expected) in [
            (above(LadderMode::Lp24), -24.0),
            (above(LadderMode::Lp12), -12.0),
            (below(LadderMode::Hp24), 24.0),
            (below(LadderMode::Hp12), 12.0),
            (above(LadderMode::Bp24), -12.0),
            (below(LadderMode::Bp24), 12.0),
            (above(LadderMode::Bp12), -6.0),
            (below(LadderMode::Bp12), 6.0),
        ] {
            assert!(
                (measured - expected).abs() < 0.1,
                "a slope of {measured} dB per octave, not {expected}"
            );
        }
        // The low-pass modes pass what is far below, the high-pass modes
        // what is far above.
        let far = |mode: LadderMode, cutoff: f64, frequency: f64| {
            let response =
                impulse_response(&mut prepared(LadderFilter::new(mode, cutoff), RATE), LEN);
            db(gain_at(&response, frequency, RATE))
        };
        assert!(far(LadderMode::Lp24, 8_000.0, 50.0).abs() < 0.01);
        assert!(far(LadderMode::Lp12, 8_000.0, 50.0).abs() < 0.01);
        assert!(far(LadderMode::Hp24, 50.0, 20_000.0).abs() < 0.01);
        assert!(far(LadderMode::Hp12, 50.0, 20_000.0).abs() < 0.01);
    }

    /// The frequency between `low` and `high` at which `gain` is largest,
    /// for a gain with a single peak there.
    fn peak_between(mut low: f64, mut high: f64, gain: impl Fn(f64) -> f64) -> f64 {
        let ratio = (5f64.sqrt() - 1.0) / 2.0;
        let (mut a, mut b) = (high - ratio * (high - low), low + ratio * (high - low));
        let (mut gain_a, mut gain_b) = (gain(a), gain(b));
        for _ in 0..40 {
            if gain_a > gain_b {
                high = b;
                (b, gain_b) = (a, gain_a);
                a = high - ratio * (high - low);
                gain_a = gain(a);
            } else {
                low = a;
                (a, gain_a) = (b, gain_b);
                b = low + ratio * (high - low);
                gain_b = gain(b);
            }
        }
        (low + high) / 2.0
    }

    #[test]
    fn resonance_raises_a_peak_at_the_cutoff() {
        const LEN: usize = 1 << 17;
        const CUTOFF: f64 = 1_000.0;
        for (resonance, closeness) in [(0.9, 0.03), (0.99, 0.003)] {
            let filter = LadderFilter::new(LadderMode::Lp24, CUTOFF).with_resonance(resonance);
            let response = impulse_response(&mut prepared(filter, RATE), LEN);
            let peak = peak_between(800.0, 1_200.0, |f| gain_at(&response, f, RATE));

            // Within 3% of the cutoff at 0.9 and 0.3% at 0.99, just below
            // it, at resonance^(1/4) times the cutoff.
            assert!(
                peak < CUTOFF && peak > CUTOFF * (1.0 - closeness),
                "peak at {peak} Hz"
            );
            let expected = CUTOFF * resonance.powf(0.25);
            assert!(
                (peak - expected).abs() < 1e-3 * CUTOFF,
                "the peak is at {peak} Hz, not {expected}"
            );
            // The same place and the same height as the formula has it.
            let formula = peak_between(800.0, 1_200.0, |f| {
                theory(LadderMode::Lp24, CUTOFF, resonance, RATE, f)
            });
            assert!((peak - formula).abs() < 1e-3, "{peak} against {formula}");
            let height = gain_at(&response, peak, RATE);
            let expected = theory(LadderMode::Lp24, CUTOFF, resonance, RATE, peak);
            assert!((height - expected).abs() < 1e-7 * expected);
            // At the cutoff itself the gain is 1 / (4 (1 - resonance)):
            // 8 dB at 0.9 and 28 dB at 0.99, over a floor 13 dB down.
            let at_cutoff = gain_at(&response, CUTOFF, RATE);
            assert!((at_cutoff - 0.25 / (1.0 - resonance)).abs() < 1e-6 * at_cutoff);
            let floor = gain_at(&response, 20.0, RATE);
            assert!(db(height / floor) > 20.0);
        }
    }

    /// The pole a ringing signal decays by, as a radius and an angle in
    /// radians per sample.
    ///
    /// A single damped sine with pole `p = r·e^(jθ)` obeys
    /// `x[n+1] = 2r·cos(θ)·x[n] - r²·x[n-1]` exactly, so a least-squares
    /// fit of that recurrence reads its pole to many digits wherever its
    /// cycles fall, which a level taken over a window does not (that is off
    /// by up to a quarter of a percent here). For a slow sine, `x[n]` and
    /// `x[n-1]` are nearly equal, so the fit is written in the second
    /// difference and the first difference, which are nearly independent:
    /// `x[n+1] - 2x[n] + x[n-1] = α·x[n] + β·(x[n] - x[n-1])`, with
    /// `r² = 1 + β` and `4r·sin²(θ/2) = -α - β²/(1 + r)²`.
    fn ringing_pole(signal: &[f64]) -> (f64, f64) {
        let (mut aa, mut ab, mut bb, mut sa, mut sb) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for three in signal.windows(3) {
            let (before, now, next) = (three[0], three[1], three[2]);
            let first = now - before;
            let second = next - 2.0 * now + before;
            aa += now * now;
            ab += now * first;
            bb += first * first;
            sa += second * now;
            sb += second * first;
        }
        let det = aa * bb - ab * ab;
        let alpha = (sa * bb - sb * ab) / det;
        let beta = (aa * sb - ab * sa) / det;
        let radius = (1.0 + beta).sqrt();
        let half = ((-alpha - beta * beta / (1.0 + radius).powi(2)) / (4.0 * radius)).sqrt();
        (radius, 2.0 * half.asin())
    }

    /// The pole of the linear ladder that rings longest:
    /// `s = -1 + (4·r)^(1/4)·e^(jπ/4)` in units of the cutoff, carried over
    /// by the bilinear transform `z = (1 + g·s) / (1 - g·s)`.
    fn ringing_pole_in_theory(resonance: f64, cutoff: f64, rate: f64) -> (f64, f64) {
        let g = (PI * cutoff / rate).tan();
        let reach = (4.0 * resonance).powf(0.25) * 0.5f64.sqrt();
        let s = Complex::new(g * (reach - 1.0), g * reach);
        let one = Complex::new(1.0, 0.0);
        let (above, below) = (one + s, one - s);
        let radius = (above.norm_sqr() / below.norm_sqr()).sqrt();
        (radius, above.arg() - below.arg())
    }

    #[test]
    fn full_resonance_is_the_edge_of_self_oscillation() {
        let ring = |resonance: f64, rate: f64, cutoff: f64| {
            let filter = LadderFilter::new(LadderMode::Lp24, cutoff).with_resonance(resonance);
            let mut filter = prepared(filter, rate);
            let mut click = AudioBuffer::<f64>::new(1, 2 * rate as usize);
            click.channel_mut(0)[0] = 1e-6;
            run_in_blocks(&mut filter, &mut click, 512);
            // From half a second on, when the other pair of poles has died
            // away, what is left is one damped sine.
            click.channel(0)[rate as usize / 2..].to_vec()
        };

        // At 1 a click rings on exactly at the cutoff and does not die away:
        // the pole is on the unit circle.
        for (rate, cutoff) in [(48_000.0, 1_000.0), (44_100.0, 5_000.0), (96_000.0, 60.0)] {
            let ringing = ring(1.0, rate, cutoff);
            assert!(strict_peak(&ringing) > 1e-10, "nothing rings");
            let (radius, angle) = ringing_pole(&ringing);
            assert!(
                (radius - 1.0).abs() < 1e-9,
                "radius {radius} at {cutoff} Hz"
            );
            let pitch = angle * rate / (2.0 * PI);
            assert!(
                (pitch / cutoff - 1.0).abs() < 1e-9,
                "it rings at {pitch} Hz, not {cutoff}"
            );
            // Which is also what the theory says for resonance 1.
            let (_, expected) = ringing_pole_in_theory(1.0, cutoff, rate);
            assert!((angle / expected - 1.0).abs() < 1e-9);
        }

        // Just below 1 it dies away, at the rate and the pitch of the pole of
        // the linear ladder.
        for (rate, cutoff) in [(48_000.0, 1_000.0), (192_000.0, 15_000.0)] {
            let resonance = 0.99;
            let (radius, angle) = ringing_pole(&ring(resonance, rate, cutoff));
            let (expected_radius, expected_angle) = ringing_pole_in_theory(resonance, cutoff, rate);
            assert!(expected_radius < 0.99999, "{expected_radius}");
            assert!(
                (radius - expected_radius).abs() < 1e-9,
                "radius {radius}, not {expected_radius}"
            );
            assert!(
                (angle / expected_angle - 1.0).abs() < 1e-8,
                "angle {angle}, not {expected_angle}"
            );
        }
    }

    #[test]
    fn drive_makes_quiet_signals_louder_and_holds_loud_ones_back() {
        const LEN: usize = 1 << 13;
        // Quiet: the gain is the drive, to the last digits.
        for drive_db in [-24.0, -6.0, 0.0, 12.0, 48.0] {
            let filter = LadderFilter::new(LadderMode::Lp24, 2_000.0).with_drive_db(drive_db);
            assert_eq!(filter.drive_db(), drive_db);
            let mut filter = prepared(filter, RATE);
            let mut level = AudioBuffer::<f64>::new(1, LEN);
            level.channel_mut(0).fill(1e-9);
            run_in_blocks(&mut filter, &mut level, 512);
            let gain = level.channel(0)[LEN - 1] / 1e-9;
            let expected = 10f64.powf(drive_db / 20.0);
            assert!(
                (gain / expected - 1.0).abs() < 1e-9,
                "{gain} for {drive_db} dB"
            );
        }

        // Loud: a tone of 100 times full scale with 48 dB of drive on top
        // comes out no larger than the saturator's 1.
        for mode in LadderMode::ALL {
            let filter = LadderFilter::new(mode, 2_000.0)
                .with_drive_db(48.0)
                .with_resonance(1.0);
            let mut filter = prepared(filter, RATE);
            let mut tone = sine::<f64>(1, 48_000, 220.0, RATE, 100.0);
            run_in_blocks(&mut filter, &mut tone, 512);
            let peak = strict_buffer_peak(&tone);
            assert!(peak <= mode.output_bound(), "{mode:?} reached {peak}");
            assert!(peak > 0.05, "{mode:?} is silent: {peak}");
        }

        // In between, a full-scale tone far below the cutoff is flattened
        // towards tanh of its level.
        let mut filter = prepared(LadderFilter::new(LadderMode::Lp24, 15_000.0), RATE);
        let mut tone = sine::<f64>(1, 9_600, 100.0, RATE, 1.0);
        run_in_blocks(&mut filter, &mut tone, 512);
        let peak = strict_peak(&tone.channel(0)[4_800..]);
        assert!((peak - 1f64.tanh()).abs() < 2e-3, "peak {peak}");
    }

    /// The largest output of each mode over `samples` samples of a signal
    /// and a modulation made by `next`, which returns the input, the
    /// cutoff, the resonance and the drive for each sample.
    fn worst_outputs(
        rate: f64,
        samples: usize,
        mut next: impl FnMut(usize) -> (f64, f64, f64, f64),
    ) -> [f64; 6] {
        let mut filters =
            LadderMode::ALL.map(|mode| prepared(LadderFilter::new(mode, 1_000.0), rate));
        let mut worst = [0.0f64; 6];
        for n in 0..samples {
            let (input, cutoff, resonance, drive_db) = next(n);
            for (filter, worst) in filters.iter_mut().zip(&mut worst) {
                filter.set_frequency(cutoff);
                filter.set_resonance(resonance);
                filter.set_drive_db(drive_db);
                let output = filter.process_sample(0, input).abs();
                *worst = if output.is_finite() {
                    worst.max(output)
                } else {
                    f64::INFINITY
                };
            }
        }
        worst
    }

    /// A square wave at half the sample rate.
    fn alternating(n: usize) -> f64 {
        if n % 2 == 0 { 1.0 } else { -1.0 }
    }

    const RATES: [f64; 5] = [22_050.0, 44_100.0, 48_000.0, 96_000.0, 192_000.0];

    #[test]
    fn up_to_a_quarter_of_the_sample_rate_the_output_stays_within_the_stated_bound() {
        const SAMPLES: usize = 20_000;
        for rate in RATES {
            let top = rate / 4.0;
            let mut random = Random::new(rate as u64);
            // Noise, full resonance and drive, a new cutoff on every sample.
            let noise = worst_outputs(rate, SAMPLES, |_| {
                let cutoff = random.log_between(1.0, top);
                (random.between(-1.0, 1.0), cutoff, 1.0, 48.0)
            });
            // The same with every setting at random, and a huge input.
            let wild = worst_outputs(rate, SAMPLES, |_| {
                let cutoff = random.log_between(1.0, top);
                let input = random.between(-1e6, 1e6);
                (input, cutoff, random.unit(), random.between(-30.0, 60.0))
            });
            // The worst input, with the cutoff thrown between its extremes.
            let thrown = worst_outputs(rate, SAMPLES, |n| {
                let cutoff = if random.unit() < 0.5 { top } else { 5.0 };
                (alternating(n / 3), cutoff, 1.0, 48.0)
            });
            for (index, mode) in LadderMode::ALL.into_iter().enumerate() {
                // Room for the rounding of the last bits only.
                let bound = mode.output_bound() * (1.0 + 1e-12);
                for (what, worst) in [("noise", noise), ("wild", wild), ("thrown", thrown)] {
                    assert!(
                        worst[index] <= bound,
                        "{mode:?} at {rate} Hz reached {} with {what}",
                        worst[index]
                    );
                }
                assert!(noise[index] > 0.25, "{mode:?} is silent");
            }
        }
    }

    #[test]
    fn it_stays_bounded_under_any_modulation_at_every_sample_rate() {
        const SAMPLES: usize = 400_000;
        // The largest output seen for each mode, as a multiple of what the
        // mode is bounded by below a quarter of the sample rate.
        let mut by_noise = [0.0f64; 6];
        let mut by_design = [0.0f64; 6];
        for rate in RATES {
            let mut random = Random::new(3 * rate as u64);
            let runs = [
                // Noise, full resonance and drive, and a new cutoff on every
                // sample, anywhere from below the lower limit to above half
                // the sample rate.
                worst_outputs(rate, SAMPLES, |_| {
                    let cutoff = random.log_between(1.0, rate);
                    (random.between(-1.0, 1.0), cutoff, 1.0, 48.0)
                }),
                // Every setting at random, and a huge input.
                worst_outputs(rate, SAMPLES, |_| {
                    let cutoff = random.log_between(1.0, rate);
                    let input = random.between(-1e6, 1e6);
                    (input, cutoff, random.unit(), random.between(-30.0, 60.0))
                }),
            ];
            let mut hold = 0;
            let mut cutoff = rate;
            let designed = [
                // Built to be the worst: a square wave at half the sample
                // rate fills the stages while the cutoff is at the top, and
                // a drop of the cutoff lets them out.
                worst_outputs(rate, SAMPLES, |n| {
                    if hold == 0 {
                        cutoff = if random.unit() < 0.5 {
                            rate
                        } else {
                            random.log_between(5.0, rate)
                        };
                        hold = 1 + (60.0 * random.unit()) as usize;
                    }
                    hold -= 1;
                    (alternating(n), cutoff, 1.0, 48.0)
                }),
                // The cutoff thrown between the limits and a quarter of the
                // sample rate on every sample.
                worst_outputs(rate, SAMPLES, |_| {
                    let cutoff = [5.0, rate, rate, rate / 4.0][(4.0 * random.unit()) as usize];
                    let input = if random.unit() < 0.5 { 1.0 } else { -1.0 };
                    (input, cutoff, 1.0, 48.0)
                }),
            ];
            for (index, mode) in LadderMode::ALL.into_iter().enumerate() {
                // The proven limit: stage n puts out at most tan(0.45 π)^n.
                let proven = hard_bound(mode);
                for run in runs.iter().chain(&designed) {
                    assert!(
                        run[index] <= proven,
                        "{mode:?} at {rate} Hz reached {}, beyond the proof's {proven}",
                        run[index]
                    );
                }
                for run in runs {
                    by_noise[index] = by_noise[index].max(run[index] / mode.output_bound());
                }
                for run in designed {
                    by_design[index] = by_design[index].max(run[index] / mode.output_bound());
                }
            }
        }
        // The measured figures the documentation quotes (3.2 times the bound
        // of a quarter of the sample rate for noise, 4.7 for the worst that
        // could be built). They are not limits, so this only guards against
        // a change that makes things worse.
        let noise = by_noise.iter().fold(0.0f64, |worst, &x| worst.max(x));
        let design = by_design.iter().fold(0.0f64, |worst, &x| worst.max(x));
        assert!(
            noise <= 3.3,
            "noise reached {noise} times the bound: {by_noise:?}"
        );
        assert!(
            design <= 4.8,
            "by design {design} times the bound: {by_design:?}"
        );
        // And the statement is not an empty one: the bound of a quarter of
        // the sample rate is exceeded above it.
        assert!(design > 4.0 && noise > 3.0, "{design}, {noise}");
    }

    /// What the proof in the comment on `tick` bounds a mode by: the sizes of
    /// its weights times `tan(0.45 π)^n` for stage `n`.
    fn hard_bound(mode: LadderMode) -> f64 {
        let top = (PI * HIGHEST_RATIO).tan();
        let mut stage = 1.0;
        let mut bound = 0.0;
        for tap in mode.taps() {
            bound += tap.abs() * stage;
            stage *= top;
        }
        // Room for the rounding of the last bits only.
        bound * (1.0 + 1e-12)
    }

    #[test]
    fn the_hard_bounds_are_the_documented_ones() {
        assert!((hard_bound(LadderMode::Lp24) - 1589.1).abs() < 0.1);
        assert!((hard_bound(LadderMode::Hp24) - 2861.3).abs() < 0.1);
        assert!(((PI * HIGHEST_RATIO).tan() - 6.3138).abs() < 1e-4);
    }

    #[test]
    fn output_does_not_depend_on_block_size() {
        type Change = fn(&mut LadderFilter<f64>);
        let render = |block: usize| {
            let filter = LadderFilter::<f64>::new(LadderMode::Lp24, 300.0)
                .with_resonance(0.8)
                .with_drive_db(12.0);
            let mut filter = filter;
            filter.prepare(&spec(RATE, 2));
            let mut signal = noise::<f64>(2, 6_000, 33);
            // The same changes on the same frames, whatever the block size.
            let changes: [(usize, Change); 4] = [
                (700, |filter| filter.set_frequency(6_000.0)),
                (1_000, |filter| {
                    filter.set_mode(LadderMode::Bp12);
                    filter.set_resonance(1.0);
                    filter.set_frequency(40.0);
                }),
                (3_333, |filter| {
                    filter.set_mode(LadderMode::Hp24);
                    filter.set_drive_db(-10.0);
                    filter.set_resonance(0.2);
                }),
                (6_000, |_| {}),
            ];
            let mut start = 0;
            for (end, change) in changes {
                signal.windowed(start..end, |part| run_in_blocks(&mut filter, part, block));
                change(&mut filter);
                start = end;
            }
            signal
        };
        let reference = render(512);
        assert!(reference.peak() > 0.1);
        for block in [1, 7, 64, 333, 6_000] {
            assert!(render(block) == reference, "blocks of {block}");
        }
    }

    #[test]
    fn no_change_of_setting_clicks() {
        // A quiet 100 Hz tone, so that every jump is the filter's doing.
        let filter = LadderFilter::new(LadderMode::Lp24, 2_000.0).with_resonance(0.3);
        let mut filter = prepared(filter, RATE);
        let mut signal = sine::<f64>(1, 24_000, 100.0, RATE, 0.05);
        let step = largest_jump(signal.channel(0));
        type Change = fn(&mut LadderFilter<f64>);
        // Each change falls on a peak of the tone, where a switch from one
        // response to another would show most.
        let changes: [(usize, Change); 6] = [
            (4_920, |filter| filter.set_mode(LadderMode::Hp24)),
            (9_720, |filter| filter.set_mode(LadderMode::Lp12)),
            (12_120, |filter| filter.set_resonance(0.9)),
            (16_920, |filter| filter.set_drive_db(20.0)),
            (19_320, |filter| filter.set_frequency(150.0)),
            (24_000, |_| {}),
        ];
        let mut start = 0;
        for (end, change) in changes {
            signal.windowed(start..end, |part| run_in_blocks(&mut filter, part, 512));
            change(&mut filter);
            start = end;
        }
        // The loudest the output gets is 10 times the tone (the drive), so
        // 10 times its steps would be no click either.
        let jump = largest_jump(signal.channel(0));
        assert!(
            jump < 12.0 * step,
            "a change jumped by {jump}, the tone by {step}"
        );

        // Taken at once, the first of these changes alone is a step of
        // nearly the whole tone.
        let filter = LadderFilter::new(LadderMode::Lp24, 2_000.0).with_resonance(0.3);
        let mut filter = prepared(filter, RATE);
        let tone = sine::<f64>(1, 9_600, 100.0, RATE, 0.05);
        let mut output = Vec::new();
        for (n, &input) in tone.channel(0).iter().enumerate() {
            if n == 4_920 {
                filter.set_mode(LadderMode::Hp24);
            }
            output.push(filter.process_sample(0, input));
        }
        assert!(largest_jump(&output) > 3.0 * step);
        assert!(largest_jump(&signal.channel(0)[4_800..9_600]) < 1.5 * step);
    }

    #[test]
    fn the_cutoff_can_be_modulated_at_audio_rate() {
        // A 300 Hz modulator sweeping the cutoff across eight octaves, from
        // 40 Hz to 10 kHz, at high resonance: the output follows without
        // running away.
        let filter = LadderFilter::new(LadderMode::Lp24, 1_000.0).with_resonance(0.95);
        let mut filter = prepared(filter, RATE);
        let mut peak = 0.0f64;
        let mut energy = 0.0;
        for n in 0..96_000 {
            let time = n as f64 / RATE;
            let cutoff = 40.0 * 2f64.powf(4.0 + 4.0 * (2.0 * PI * 300.0 * time).sin());
            filter.set_frequency(cutoff);
            let output = filter.process_sample(0, 0.25 * (2.0 * PI * 110.0 * time).sin());
            peak = peak.max(output.abs());
            energy += output * output;
        }
        assert!(peak <= 1.0, "peak {peak}");
        assert!(energy > 1.0, "nothing came out");
    }

    #[test]
    fn a_change_of_mode_is_bounded_by_the_larger_bound_until_it_arrives() {
        // A 2 kHz square wave through Hp12 at its cutoff of 2 kHz, with
        // high resonance and drive, then a change to Lp24 on block 150 of
        // 64 frames: for the 20 ms glide (15 blocks) the output is held by
        // the larger of the two bounds, 4, not by Lp24's 1, which it does
        // exceed; once the glide has arrived, by 1.
        let mut filter = LadderFilter::<f64>::new(LadderMode::Hp12, 2_000.0)
            .with_resonance(0.95)
            .with_drive_db(24.0);
        filter.prepare(&ProcessSpec {
            sample_rate: RATE,
            max_block_size: 64,
            num_channels: 1,
        });
        let mut block = AudioBuffer::<f64>::new(1, 64);
        let (mut gliding, mut arrived) = (0.0f64, 0.0f64);
        for index in 0..250 {
            if index == 150 {
                filter.set_mode(LadderMode::Lp24);
            }
            for (i, sample) in block.channel_mut(0).iter_mut().enumerate() {
                *sample = if ((index * 64 + i) / 12) % 2 == 0 {
                    1.0
                } else {
                    -1.0
                };
            }
            filter.process(&mut block);
            let peak = strict_buffer_peak(&block);
            if (150..165).contains(&index) {
                assert_eq!(filter.mode(), LadderMode::Lp24);
                gliding = gliding.max(peak);
            } else if index >= 165 {
                arrived = arrived.max(peak);
            }
        }
        let larger = LadderMode::Hp12
            .output_bound()
            .max(LadderMode::Lp24.output_bound());
        assert!(gliding <= larger && gliding > 1.0, "{gliding}");
        assert!(arrived <= LadderMode::Lp24.output_bound(), "{arrived}");
        assert!(arrived > 0.1, "{arrived}");
    }

    #[test]
    fn fully_open_the_low_pass_is_down_by_the_stated_amount() {
        // The cutoff stops at 0.45 of the sample rate: 19.8 kHz at 44.1 kHz.
        const RATE: f64 = 44_100.0;
        let filter = LadderFilter::new(LadderMode::Lp24, 1e9);
        assert_eq!(filter.frequency(), 0.45 * RATE);
        let response = impulse_response(&mut prepared(filter, RATE), 1 << 12);
        let at = |frequency: f64| db(gain_at(&response, frequency, RATE));
        assert!((at(15_000.0) + 1.4).abs() < 0.05, "{}", at(15_000.0));
        assert!((at(18_000.0) + 4.4).abs() < 0.05, "{}", at(18_000.0));
        // As the formula has it.
        let theory = db(theory(LadderMode::Lp24, 0.45 * RATE, 0.0, RATE, 18_000.0));
        assert!((at(18_000.0) - theory).abs() < 1e-9);
    }

    #[test]
    fn channels_are_independent_and_reset_clears_them() {
        let mut filter = LadderFilter::<f32>::new(LadderMode::Lp24, 200.0).with_resonance(1.0);
        filter.prepare(&spec(RATE, 2));
        let mut buffer = AudioBuffer::<f32>::new(2, 32);
        buffer.channel_mut(0).fill(1.0);
        filter.process(&mut buffer);
        assert!(buffer.channel(0)[31] > 0.0);
        assert_eq!(buffer.channel(1), &[0.0; 32]);

        filter.reset();
        buffer.clear();
        filter.process(&mut buffer);
        assert_eq!(buffer.peak(), 0.0);
    }

    #[test]
    fn it_survives_garbage_and_use_before_it_is_prepared() {
        // Not prepared: there is no memory for any channel, so audio passes
        // untouched, and nothing panics.
        let mut filter = LadderFilter::<f32>::new(LadderMode::Bp24, 100.0);
        assert_eq!(filter.mode(), LadderMode::Bp24);
        let mut buffer = noise::<f32>(2, 64, 1);
        let before = buffer.clone();
        filter.process(&mut buffer);
        assert!(buffer == before);
        assert_eq!(filter.process_sample(0, 0.25), 0.25);

        filter.prepare(&spec(RATE, 1));
        assert_eq!(filter.process_sample(7, 0.25), 0.25);

        // Settings that make no sense are limited.
        for value in [f64::NAN, -1.0, 0.0, f64::INFINITY, f64::NEG_INFINITY, 1e300] {
            filter.set_frequency(value);
            filter.set_resonance(value);
            filter.set_drive_db(value);
            assert!((5.0..=0.45 * RATE).contains(&filter.frequency()));
            assert!((0.0..=1.0).contains(&filter.resonance()));
            assert!((-24.0..=48.0).contains(&filter.drive_db()));
            // More channels and more frames than prepared for.
            let mut block = noise::<f32>(3, 2_000, 3);
            filter.process(&mut block);
            let bound = 4.5 * LadderMode::Bp24.output_bound();
            assert!(strict_peak(block.channel(0)) <= bound);
        }
        assert_eq!(filter.resonance(), 1.0);
        assert_eq!(filter.drive_db(), 48.0);

        // Samples that are not numbers leave no trace: the saturator stops
        // them, and the filter carries on once they are gone.
        let filter = LadderFilter::new(LadderMode::Lp24, 1_000.0).with_resonance(0.5);
        let mut filter = prepared(filter, RATE);
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            for _ in 0..10 {
                assert!(filter.process_sample(0, bad).abs() <= 1.0);
            }
        }
        let mut last = 0.0;
        for _ in 0..4_800 {
            last = filter.process_sample(0, 1e-3);
        }
        assert!((last - 1e-3 / 3.0).abs() < 1e-9, "it settled at {last}");

        // A block with no channel still moves the glides on.
        filter.set_frequency(10_000.0);
        filter.set_resonance(0.0);
        let mut empty = AudioBuffer::<f64>::new(0, 960);
        filter.process(&mut empty);
        filter.reset();
        let mut impulse = AudioBuffer::<f64>::new(1, 8);
        impulse.channel_mut(0)[0] = 1e-6;
        filter.process(&mut impulse);
        let g = (PI * 10_000.0 / RATE).tan();
        let expected = 1e-6 * (g / (1.0 + g)).powi(4);
        assert!((impulse.channel(0)[0] - expected).abs() < 1e-9 * expected);
    }
}
