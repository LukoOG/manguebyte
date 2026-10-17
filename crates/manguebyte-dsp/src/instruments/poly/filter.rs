//! The poly synth's filters: every model as a kernel on one channel's
//! memory, and a slot that runs the model a filter is set to (and, for a
//! moment after it changes, the one before).
//!
//! Every model is built from trapezoidal integrators, so its cutoff can
//! move at audio rate without the filter blowing up or clicking, and,
//! quiet enough for its saturation to stay out of the way, each has the
//! response of its analogue prototype through the bilinear transform:
//! the ladder's is [`LadderResponse`](crate::curve::LadderResponse)'s, the
//! state-variable, SEM-like and MS-20-like filters' are
//! [`SvfResponse`](crate::curve::SvfResponse)'s (the tests check both).

use core::f64::consts::PI;

use super::super::common::flush;
use super::params::FilterModel;
use super::tables::VOWELS;

/// The cutoff limits, in hertz and as a share of the sample rate the
/// filter runs at.
const LOWEST_HZ: f64 = 5.0;
const HIGHEST_RATIO: f64 = 0.45;

/// How much of the bass the ladder's resonance takes away is given back:
/// the low-pass modes are multiplied by `1 + LADDER_MAKE_UP·k`, so that at
/// full resonance the bass is 4.4 dB down rather than 14.
const LADDER_MAKE_UP: f64 = 0.5;

/// The size the SEM-like filter's band-pass memory is softly held to.
const SEM_LIMIT: f64 = 4.0;

/// The longest delay a comb can need, as a share of the rate: its lowest
/// cutoff is a tenth of a hertz above this many cycles per sample.
pub(super) const COMB_LOWEST_HZ: f64 = 10.0;

/// The combs' feedback at full resonance.
const COMB_FEEDBACK: f64 = 0.985;

/// What a model family is made of, so that two models of one family share
/// their memory and a change between them needs no second kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Family {
    Off,
    Ladder,
    Svf,
    Sem,
    SkLow,
    SkHigh,
    Comb,
    Formant,
}

impl Family {
    pub(super) fn of(model: FilterModel) -> Self {
        match model {
            FilterModel::Off => Self::Off,
            FilterModel::LadderLp24
            | FilterModel::LadderLp12
            | FilterModel::LadderHp24
            | FilterModel::LadderBp12 => Self::Ladder,
            FilterModel::SvfLp
            | FilterModel::SvfBp
            | FilterModel::SvfHp
            | FilterModel::SvfNotch => Self::Svf,
            FilterModel::SemNotch | FilterModel::SemBand => Self::Sem,
            FilterModel::Ms20Lp => Self::SkLow,
            FilterModel::Ms20Hp => Self::SkHigh,
            FilterModel::CombPlus | FilterModel::CombMinus => Self::Comb,
            FilterModel::Formant => Self::Formant,
        }
    }
}

/// The settings of a filter for one sample, as its model computes with
/// them.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Coefficients {
    /// `g / (1 + g)` for the integrator gain `g = tan(π·fc/fs)`.
    big_g: f64,
    /// The integrator gain.
    g: f64,
    /// Feedback or damping, as the family uses it.
    k: f64,
    /// The gain into the saturation.
    drive: f64,
    /// How much of the input saturation is in use, 0 to 1.
    shaping: f64,
    /// The trapezoidal state-variable filter's `a1`, `a2`, `a3`.
    svf: [f64; 3],
    /// For the formant filter, each formant's `a1`, `a2`, `a3`, `k` and
    /// level.
    formants: [[f64; 5]; 3],
    /// The comb's delay in samples, its feedback and its damping.
    comb: [f64; 3],
    /// The model's own setting: SEM mode, vowel, damping.
    morph: f64,
}

/// The integrator gain for a cutoff at `rate`, within the limits.
#[inline]
pub(super) fn integrator_gain(cutoff: f64, rate: f64) -> f64 {
    let cutoff = crate::numeric::limit(cutoff, LOWEST_HZ, HIGHEST_RATIO * rate);
    (PI * cutoff / rate).tan()
}

/// The state-variable filter's coefficients for gain `g` and damping `k`.
#[inline]
fn svf_coefficients(g: f64, k: f64) -> [f64; 3] {
    let a1 = 1.0 / (1.0 + g * (g + k));
    let a2 = g * a1;
    [a1, a2, g * a2]
}

/// The Q of the state-variable filter at a resonance, 0.5 to 20.
#[inline]
pub(super) fn svf_q(resonance: f64) -> f64 {
    0.5 * 40f64.powf(resonance)
}

/// The Q of the SEM-like filter at a resonance, 0.5 to 10.
#[inline]
pub(super) fn sem_q(resonance: f64) -> f64 {
    0.5 * 20f64.powf(resonance)
}

/// The feedback of the MS-20-like loop at a resonance: the loop is
/// `1/(s² + (2 - k)s + 1)`, so a Q of `1/(2 - k)`, and it oscillates at 2.
#[inline]
pub(super) fn sk_feedback(resonance: f64) -> f64 {
    2.0 * resonance
}

impl Coefficients {
    /// Nothing worked out yet.
    pub(super) const ZERO: Self = Self {
        big_g: 0.0,
        g: 0.0,
        k: 0.0,
        drive: 1.0,
        shaping: 0.0,
        svf: [0.0; 3],
        formants: [[0.0; 5]; 3],
        comb: [0.0; 3],
        morph: 0.0,
    };

    /// The coefficients of `model` at `cutoff` hertz, `resonance` (0 to 1),
    /// `drive_db` and `morph`, running at `rate`.
    pub(super) fn new(
        model: FilterModel,
        cutoff: f64,
        resonance: f64,
        drive_db: f64,
        morph: f64,
        rate: f64,
    ) -> Self {
        let mut c = Self {
            drive: (drive_db * (core::f64::consts::LN_10 / 20.0)).exp(),
            shaping: (drive_db / 3.0).clamp(0.0, 1.0),
            morph,
            ..Self::default()
        };
        match Family::of(model) {
            Family::Off => {}
            Family::Ladder => {
                c.g = integrator_gain(cutoff, rate);
                c.big_g = c.g / (1.0 + c.g);
                c.k = 4.0 * resonance;
            }
            Family::Svf | Family::Sem => {
                c.g = integrator_gain(cutoff, rate);
                let q = if Family::of(model) == Family::Svf {
                    svf_q(resonance)
                } else {
                    sem_q(resonance)
                };
                c.k = 1.0 / q;
                c.svf = svf_coefficients(c.g, c.k);
            }
            Family::SkLow | Family::SkHigh => {
                c.g = integrator_gain(cutoff, rate);
                c.big_g = c.g / (1.0 + c.g);
                c.k = sk_feedback(resonance);
            }
            Family::Comb => {
                let cutoff = crate::numeric::limit(cutoff, COMB_LOWEST_HZ + 0.1, 0.25 * rate);
                let feedback = COMB_FEEDBACK * resonance.sqrt();
                let feedback = if model == FilterModel::CombMinus {
                    -feedback
                } else {
                    feedback
                };
                let damping = 0.95 * morph;
                // The damping's one-pole delays the fundamental by its
                // phase delay; the delay line gives it back.
                let omega = 2.0 * PI * cutoff / rate;
                let lag = (damping * omega.sin()).atan2(1.0 - damping * omega.cos()) / omega;
                c.comb = [(rate / cutoff - lag).max(2.0), feedback, damping];
            }
            Family::Formant => {
                let shift = (cutoff / 1_000.0).clamp(0.25, 4.0);
                let place = morph.clamp(0.0, 1.0) * (VOWELS.len() - 1) as f64;
                let from = (place as usize).min(VOWELS.len() - 2);
                let blend = place - from as f64;
                for (slot, (a, b)) in c
                    .formants
                    .iter_mut()
                    .zip(VOWELS[from].iter().zip(&VOWELS[from + 1]))
                {
                    let centre = (a.0.ln() * (1.0 - blend) + b.0.ln() * blend).exp() * shift;
                    let bandwidth = (a.1 * (1.0 - blend) + b.1 * blend) * shift;
                    let level = a.2 * (1.0 - blend) + b.2 * blend;
                    let q = centre / bandwidth * (0.5 + 2.5 * resonance);
                    let g = integrator_gain(centre, rate);
                    let k = 1.0 / q;
                    let [a1, a2, a3] = svf_coefficients(g, k);
                    // Each formant's level rises as the formants narrow,
                    // less than the narrowing takes away.
                    *slot = [a1, a2, a3, k, level.sqrt() * 2.0];
                }
            }
        }
        c
    }
}

/// The memory of every model on one channel.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Memory {
    ladder: [f64; 4],
    svf: [f64; 2],
    sem: [f64; 2],
    sk_low: [f64; 3],
    sk_high: [f64; 3],
    formants: [[f64; 2]; 3],
    comb_damping: f64,
}

impl Memory {
    /// Clears the memory of one family.
    fn clear(&mut self, family: Family) {
        match family {
            Family::Off | Family::Comb => self.comb_damping = 0.0,
            Family::Ladder => self.ladder = [0.0; 4],
            Family::Svf => self.svf = [0.0; 2],
            Family::Sem => self.sem = [0.0; 2],
            Family::SkLow => self.sk_low = [0.0; 3],
            Family::SkHigh => self.sk_high = [0.0; 3],
            Family::Formant => self.formants = [[0.0; 2]; 3],
        }
    }
}

/// The input saturation of the models that have none of their own: a
/// `tanh` with the drive's gain, blended in over the first 3 dB so that
/// 0 dB is exactly linear, and the level brought back down by the square
/// root of the gain.
#[inline]
fn shaped(x: f64, c: &Coefficients) -> f64 {
    if c.shaping <= 0.0 {
        x
    } else {
        let wet = fast_tanh(c.drive * x) / c.drive.sqrt();
        x + (wet - x) * c.shaping
    }
}

/// `tanh`, from the [7/6] Padé approximant of Lambert's continued
/// fraction, `x(135135 + 17325x² + 378x⁴ + x⁶) / (135135 + 62370x² +
/// 3150x⁴ + 28x⁶)`, held at ±1 from where it reaches 1 (|x| = 4.97). It is
/// within 2·10⁻⁶ of `tanh` up to |x| = 3 and 1.2·10⁻⁴ anywhere, and agrees
/// with it to the seventh power of `x` near 0, so a quiet signal sees the
/// same filter; it is several times cheaper than the library's `tanh`, which
/// the loops below evaluate a few times a sample.
#[inline]
pub(super) fn fast_tanh(x: f64) -> f64 {
    let x2 = x * x;
    if x2.is_nan() {
        return 0.0;
    }
    if x2 >= 4.97 * 4.97 {
        return if x > 0.0 { 1.0 } else { -1.0 };
    }
    let numerator = x * (135_135.0 + x2 * (17_325.0 + x2 * (378.0 + x2)));
    let denominator = 135_135.0 + x2 * (62_370.0 + x2 * (3_150.0 + x2 * 28.0));
    (numerator / denominator).clamp(-1.0, 1.0)
}

/// Solves `v + b·tanh(v) = a` for `v` and returns `tanh(v)`, for `b` of
/// zero or more: the ladder's loop, by Newton's method from the solution
/// without the saturator, as [`LadderFilter`](crate::LadderFilter) does with
/// the library's `tanh`.
/// Not a number counts as zero.
#[inline]
fn ladder_loop(a: f64, b: f64) -> f64 {
    let target = crate::numeric::limit(a.abs(), 0.0, 1e30);
    let mut v = target / (1.0 + b);
    let mut t = fast_tanh(v);
    for _ in 0..12 {
        let step = (v + b * t - target) / (1.0 + b * (1.0 - t * t));
        v -= step;
        t = fast_tanh(v);
        // The error left after a step is about its square.
        if step.abs() <= 1e-7 * v {
            break;
        }
    }
    if a < 0.0 { -t } else { t }
}

/// One sample of the transistor ladder: four trapezoidal one-poles with
/// the fourth fed back to a `tanh` in front of the first, the loop solved
/// without delay (V. Zavalishin, "The Art of VA Filter Design", chapters 4
/// to 6, as [`LadderFilter`](crate::LadderFilter) does). Returns the input
/// of the first stage and the outputs of the four.
#[inline]
fn ladder(state: &mut [f64; 4], x: f64, c: &Coefficients) -> [f64; 5] {
    let gain = c.big_g;
    let rest = 1.0 - gain;
    let gain2 = gain * gain;
    let decided = rest * (gain2 * gain * state[0] + gain2 * state[1] + gain * state[2] + state[3]);
    let u = ladder_loop(c.drive * x - c.k * decided, c.k * gain2 * gain2);
    let mut taps = [u, 0.0, 0.0, 0.0, 0.0];
    let mut signal = u;
    for (index, memory) in state.iter_mut().enumerate() {
        let v = (signal - *memory) * gain;
        signal = v + *memory;
        *memory = flush(signal + v);
        taps[index + 1] = signal;
    }
    taps
}

/// One sample of the trapezoidal state-variable filter (A. Simper,
/// "Linear trapezoidal integrated SVF", Cytomic 2013): its low-pass,
/// band-pass at unity gain, high-pass, and the band-pass memory before
/// scaling, `v1`.
#[inline]
fn svf(state: &mut [f64; 2], x: f64, a: [f64; 3], k: f64) -> (f64, f64, f64) {
    let [ic1, ic2] = *state;
    let v3 = x - ic2;
    let v1 = a[0] * ic1 + a[1] * v3;
    let v2 = ic2 + a[1] * ic1 + a[2] * v3;
    state[0] = flush(2.0 * v1 - ic1);
    state[1] = flush(2.0 * v2 - ic2);
    (v2, k * v1, x - k * v1 - v2)
}

/// A soft limit to about `limit`: `x / √(1 + (x/limit)²)`, the identity
/// for small values.
#[inline]
fn soft_limit(x: f64, limit: f64) -> f64 {
    let ratio = x / limit;
    x / (1.0 + ratio * ratio).sqrt()
}

/// Solves `u = tanh(c + b·u)` for `u`, for `b` from 0 to 1/2, by Newton's
/// method. The left side less the right grows at a rate of at least
/// `1 - b`, so there is one solution and the method converges fast from
/// the solution of the linear loop.
#[inline]
fn loop_solution(c: f64, b: f64) -> f64 {
    let c = crate::numeric::limit(c, -1e6, 1e6);
    let mut u = fast_tanh(c / (1.0 - b));
    for _ in 0..8 {
        let t = fast_tanh(c + b * u);
        let step = (u - t) / (1.0 - b * (1.0 - t * t));
        u -= step;
        if step.abs() < 1e-9 {
            break;
        }
    }
    u.clamp(-1.0, 1.0)
}

/// One sample of the MS-20-like low-pass: two trapezoidal one-poles in a
/// row with the output fed back, high-passed by a third, to a saturating
/// node between them. Linearised, `1/(s² + (2 - k)s + 1)`: the Sallen–Key
/// low-pass of the Korg 35 circuit. The memories are the three
/// one-poles'.
#[inline]
fn sk_low(state: &mut [f64; 3], x: f64, c: &Coefficients) -> f64 {
    let gain = c.big_g;
    let rest = 1.0 - gain;
    let [s1, s2, s3] = *state;
    let input = c.drive * x;
    let a = gain * input + rest * s1;
    // y = G·u + (1 - G)·s2 and the feedback is (1 - G)·(y - s3), so
    // u = tanh(cc + b·u).
    let b = c.k * rest * gain;
    let cc = a + c.k * rest * (rest * s2 - s3);
    let u = loop_solution(cc, b);
    let y = gain * u + rest * s2;
    let high = rest * (y - s3);
    let low3 = y - high;
    *state = [
        flush(2.0 * a - s1),
        flush(2.0 * y - s2),
        flush(2.0 * low3 - s3),
    ];
    y
}

/// The MS-20-like high-pass: the same loop with the roles of low- and
/// high-pass swapped, linearised `s²/(s² + (2 - k)s + 1)`.
#[inline]
fn sk_high(state: &mut [f64; 3], x: f64, c: &Coefficients) -> f64 {
    let gain = c.big_g;
    let rest = 1.0 - gain;
    let [s1, s2, s3] = *state;
    let input = c.drive * x;
    let a = rest * (input - s1);
    let low1 = input - a;
    // y = (1 - G)·(u - s2) and the feedback is G·y + (1 - G)·s3.
    let b = c.k * gain * rest;
    let cc = a + c.k * (rest * s3 - gain * rest * s2);
    let u = loop_solution(cc, b);
    let y = rest * (u - s2);
    let low2 = u - y;
    let f = gain * y + rest * s3;
    *state = [
        flush(2.0 * low1 - s1),
        flush(2.0 * low2 - s2),
        flush(2.0 * f - s3),
    ];
    y
}

/// A filter: the model it is set to and the memory of every model, on
/// two channels, and the delay lines of the comb.
#[derive(Debug, Clone)]
pub(super) struct FilterSlot {
    memory: [Memory; 2],
    comb: [Vec<f64>; 2],
    written: usize,
    model: FilterModel,
    /// The model before the last change, and how far the change has gone
    /// (1: done).
    previous: FilterModel,
    blend: f64,
}

impl FilterSlot {
    pub(super) fn new() -> Self {
        Self {
            memory: [Memory::default(); 2],
            comb: [Vec::new(), Vec::new()],
            written: 0,
            model: FilterModel::Off,
            previous: FilterModel::Off,
            blend: 1.0,
        }
    }

    /// Makes room for the comb's longest delay at `rate`. Allocates.
    pub(super) fn prepare(&mut self, rate: f64) {
        let len = ((rate / COMB_LOWEST_HZ) as usize + 8).next_power_of_two();
        for line in &mut self.comb {
            *line = vec![0.0; len];
        }
        self.silence();
    }

    /// Back to the state of a new filter, for `model`. Only what the comb
    /// has written since it was last cleared is cleared, so that a note
    /// that did not use it costs nothing here.
    pub(super) fn silence(&mut self) {
        self.memory = [Memory::default(); 2];
        for line in &mut self.comb {
            let used = self.written.min(line.len());
            line[..used].fill(0.0);
        }
        self.written = 0;
        self.previous = self.model;
        self.blend = 1.0;
    }

    /// The model, as set.
    pub(super) fn model(&self) -> FilterModel {
        self.model
    }

    /// The model before the last change, while the change is still
    /// blending.
    pub(super) fn fading_from(&self) -> Option<FilterModel> {
        (self.blend < 1.0).then_some(self.previous)
    }

    /// Takes a model. On a sounding note (`blend_now` false) the change is
    /// blended over the following samples; otherwise it is at once.
    pub(super) fn set_model(&mut self, model: FilterModel, at_once: bool) {
        if model == self.model {
            return;
        }
        let family = Family::of(model);
        if at_once {
            self.model = model;
            self.previous = model;
            self.blend = 1.0;
            return;
        }
        if family != Family::of(self.model) {
            // The new family starts from rest, unless it is the one being
            // faded out.
            if self.blend >= 1.0 || Family::of(self.previous) != family {
                for memory in &mut self.memory {
                    memory.clear(family);
                }
            }
        }
        self.previous = self.model;
        self.model = model;
        self.blend = 0.0;
    }

    /// Moves the blend between models on by `step`.
    pub(super) fn glide(&mut self, step: f64) {
        if self.blend < 1.0 {
            self.blend = (self.blend + step).min(1.0);
        }
    }

    /// Copies the first channel's memory to the second, for a filter that
    /// has run on one channel and is to run on two.
    pub(super) fn spread_to_stereo(&mut self) {
        self.memory[1] = self.memory[0];
        let [first, second] = &mut self.comb;
        let used = self.written.min(first.len()).min(second.len());
        second[..used].copy_from_slice(&first[..used]);
    }

    /// One sample of `channel` through the filter. `now` holds the
    /// coefficients of the model, `before` those of the model being faded
    /// out, if any. `through` says what [`FilterModel::Off`] puts out: the
    /// input, in series, or nothing, in parallel. `advance` moves the
    /// comb's write position on, which the last channel does.
    #[inline]
    pub(super) fn process(
        &mut self,
        channel: usize,
        x: f64,
        now: &Coefficients,
        before: &Coefficients,
        advance: bool,
        through: bool,
    ) -> f64 {
        let model = self.model;
        let wire = if through { x } else { 0.0 };
        if self.blend >= 1.0 {
            return self.run(channel, x, wire, model, now, advance, None);
        }
        let previous = self.previous;
        if Family::of(previous) == Family::of(model) {
            // One kernel; only the output taps differ.
            let mut other = 0.0;
            let new = self.run(
                channel,
                x,
                wire,
                model,
                now,
                advance,
                Some((previous, &mut other)),
            );
            other + (new - other) * self.blend
        } else {
            let new = self.run(channel, x, wire, model, now, advance, None);
            let old = self.run(channel, x, wire, previous, before, false, None);
            old + (new - old) * self.blend
        }
    }

    /// Runs `model`'s kernel on `channel`; [`FilterModel::Off`] puts out
    /// `wire`. `also` asks for the output of another model of the same
    /// family from the same run.
    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn run(
        &mut self,
        channel: usize,
        x: f64,
        wire: f64,
        model: FilterModel,
        c: &Coefficients,
        advance: bool,
        also: Option<(FilterModel, &mut f64)>,
    ) -> f64 {
        let memory = &mut self.memory[channel];
        let pick = |model: FilterModel, taps: &[f64; 5]| -> f64 { ladder_output(model, taps, c) };
        match Family::of(model) {
            Family::Off => {
                if let Some((_, other)) = also {
                    *other = wire;
                }
                wire
            }
            Family::Ladder => {
                let taps = ladder(&mut memory.ladder, x, c);
                if let Some((other_model, other)) = also {
                    *other = pick(other_model, &taps);
                }
                pick(model, &taps)
            }
            Family::Svf => {
                let (low, band, high) = svf(&mut memory.svf, shaped(x, c), c.svf, c.k);
                let output = |model: FilterModel| match model {
                    FilterModel::SvfBp => band,
                    FilterModel::SvfHp => high,
                    FilterModel::SvfNotch => low + high,
                    _ => low,
                };
                if let Some((other_model, other)) = also {
                    *other = output(other_model);
                }
                output(model)
            }
            Family::Sem => {
                let (low, band, high) = svf(&mut memory.sem, shaped(x, c), c.svf, c.k);
                memory.sem[0] = soft_limit(memory.sem[0], SEM_LIMIT);
                let output = |model: FilterModel| sem_output(model, c.morph, low, band, high);
                if let Some((other_model, other)) = also {
                    *other = output(other_model);
                }
                output(model)
            }
            Family::SkLow => {
                let y = sk_low(&mut memory.sk_low, x, c) / c.drive.max(1.0).sqrt();
                if let Some((_, other)) = also {
                    *other = y;
                }
                y
            }
            Family::SkHigh => {
                let y = sk_high(&mut memory.sk_high, x, c) / c.drive.max(1.0).sqrt();
                if let Some((_, other)) = also {
                    *other = y;
                }
                y
            }
            Family::Comb => {
                let y = self.comb(channel, shaped(x, c), c, advance);
                if let Some((_, other)) = also {
                    *other = y;
                }
                y
            }
            Family::Formant => {
                let input = shaped(x, c);
                let mut sum = 0.0;
                for (state, f) in memory.formants.iter_mut().zip(&c.formants) {
                    let (_, band, _) = svf(state, input, [f[0], f[1], f[2]], f[3]);
                    sum += band * f[4];
                }
                if let Some((_, other)) = also {
                    *other = sum;
                }
                sum
            }
        }
    }

    /// One sample of the comb: `y = x + feedback·damped(y delayed)`, the
    /// delay read between samples by cubic Lagrange interpolation, and the
    /// output scaled by `√(1 - feedback²)`, which keeps the level of a
    /// broad input whatever the feedback.
    #[inline]
    fn comb(&mut self, channel: usize, x: f64, c: &Coefficients, advance: bool) -> f64 {
        let line = &mut self.comb[channel];
        let len = line.len();
        if len < 8 {
            return x;
        }
        let mask = len - 1;
        let [delay, feedback, damping] = c.comb;
        let delay = delay.clamp(2.0, (len - 4) as f64);
        let whole = delay.floor();
        let t = delay - whole;
        let newest = self.written.wrapping_sub(whole as usize);
        let at = |back: usize| line[newest.wrapping_sub(back).wrapping_add(1) & mask];
        // Points at delays whole - 1, whole, whole + 1, whole + 2.
        let (p0, p1, p2, p3) = (at(0), at(1), at(2), at(3));
        // Lagrange through -1, 0, 1, 2 at t.
        let c1 = p2 - p0 / 3.0 - p1 / 2.0 - p3 / 6.0;
        let c2 = 0.5 * (p0 + p2) - p1;
        let c3 = (p3 - p0) / 6.0 + 0.5 * (p1 - p2);
        let delayed = p1 + t * (c1 + t * (c2 + t * c3));
        let memory = &mut self.memory[channel];
        memory.comb_damping = flush(delayed + (memory.comb_damping - delayed) * damping);
        let y = x + feedback * memory.comb_damping;
        let y = crate::numeric::limit(y, -64.0, 64.0);
        line[self.written & mask] = y;
        if advance {
            self.written = self.written.wrapping_add(1);
        }
        y * (1.0 - feedback * feedback).sqrt()
    }
}

/// The ladder's output for `model` from the input of its first stage and
/// the outputs of its four: the weights of
/// [`LadderMode`](crate::LadderMode)'s modes, with the bass given back to
/// the low-passes and the drive's gain taken back out above unity.
#[inline]
fn ladder_output(model: FilterModel, taps: &[f64; 5], c: &Coefficients) -> f64 {
    let [u, y1, y2, y3, y4] = *taps;
    let raw = match model {
        FilterModel::LadderLp12 => y2 * (1.0 + LADDER_MAKE_UP * c.k),
        FilterModel::LadderHp24 => u - 4.0 * y1 + 6.0 * y2 - 4.0 * y3 + y4,
        FilterModel::LadderBp12 => 2.0 * (y1 - y2),
        _ => y4 * (1.0 + LADDER_MAKE_UP * c.k),
    };
    raw / c.drive.max(1.0).sqrt()
}

/// The SEM-like filter's output at `morph`: low-pass to notch to
/// high-pass, or low-pass to band-pass to high-pass, in straight lines.
#[inline]
fn sem_output(model: FilterModel, morph: f64, low: f64, band: f64, high: f64) -> f64 {
    let m = morph.clamp(0.0, 1.0);
    if model == FilterModel::SemBand {
        if m < 0.5 {
            let t = 2.0 * m;
            low + (band - low) * t
        } else {
            let t = 2.0 * m - 1.0;
            band + (high - band) * t
        }
    } else {
        low + (high - low) * m
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::curve::{FrequencyResponse, LadderResponse, SvfResponse};
    use crate::testing::gain_at;
    use crate::{LadderMode, SvfMode};

    const RATE: f64 = 96_000.0;

    /// The response of `model` to an impulse too quiet to saturate
    /// anything, scaled back to an impulse of 1.
    pub(in super::super) fn impulse_response(
        model: FilterModel,
        cutoff: f64,
        resonance: f64,
        morph: f64,
        len: usize,
    ) -> Vec<f64> {
        const LEVEL: f64 = 1e-7;
        let mut slot = FilterSlot::new();
        slot.prepare(RATE);
        slot.set_model(model, true);
        let c = Coefficients::new(model, cutoff, resonance, 0.0, morph, RATE);
        (0..len)
            .map(|n| {
                let x = if n == 0 { LEVEL } else { 0.0 };
                slot.process(0, x, &c, &c, true, true) / LEVEL
            })
            .collect()
    }

    fn check(
        model: FilterModel,
        cutoff: f64,
        resonance: f64,
        morph: f64,
        expected: &dyn FrequencyResponse,
    ) -> f64 {
        let response = impulse_response(model, cutoff, resonance, morph, 1 << 16);
        let tail = response[response.len() - 64..]
            .iter()
            .fold(0.0f64, |m, x| m.max(x.abs()));
        assert!(tail < 1e-9, "{model:?}: the response is cut off ({tail})");
        let mut worst = 0.0f64;
        for frequency in [
            20.0,
            100.0,
            0.5 * cutoff,
            0.9 * cutoff,
            cutoff,
            1.1 * cutoff,
            2.0 * cutoff,
            15_000.0,
        ] {
            let measured = gain_at(&response, frequency, RATE);
            let wanted = expected.response_at(frequency).norm();
            let error = (measured - wanted).abs() / wanted.max(1e-3);
            worst = worst.max(error);
            assert!(
                error < 1e-5,
                "{model:?} at {cutoff} Hz, resonance {resonance}, morph {morph}, {frequency} Hz: \
                 {measured} against {wanted}"
            );
        }
        worst
    }

    /// The linear part of the ladder's make-up gain, which the curve does
    /// not include.
    struct Scaled<R>(R, f64);
    impl<R: FrequencyResponse> FrequencyResponse for Scaled<R> {
        fn sample_rate(&self) -> f64 {
            self.0.sample_rate()
        }
        fn response_at(&self, frequency: f64) -> crate::fft::Complex<f64> {
            self.0.response_at(frequency).scale(self.1)
        }
    }

    #[test]
    fn quiet_filters_have_the_responses_the_curves_draw() {
        let mut worst = 0.0f64;
        for cutoff in [250.0, 2_000.0] {
            for resonance in [0.0, 0.4, 0.8] {
                let k = 4.0 * resonance;
                for (model, mode, make_up) in [
                    (
                        FilterModel::LadderLp24,
                        LadderMode::Lp24,
                        1.0 + LADDER_MAKE_UP * k,
                    ),
                    (
                        FilterModel::LadderLp12,
                        LadderMode::Lp12,
                        1.0 + LADDER_MAKE_UP * k,
                    ),
                    (FilterModel::LadderHp24, LadderMode::Hp24, 1.0),
                    (FilterModel::LadderBp12, LadderMode::Bp12, 1.0),
                ] {
                    let curve = LadderResponse::new(mode, cutoff, resonance, 0.0, RATE);
                    worst = worst.max(check(
                        model,
                        cutoff,
                        resonance,
                        0.0,
                        &Scaled(curve, make_up),
                    ));
                }
                let q = svf_q(resonance);
                for (model, mode) in [
                    (FilterModel::SvfLp, SvfMode::Lowpass),
                    (FilterModel::SvfBp, SvfMode::Bandpass),
                    (FilterModel::SvfHp, SvfMode::Highpass),
                    (FilterModel::SvfNotch, SvfMode::Notch),
                ] {
                    let curve = SvfResponse::new(mode, cutoff, q, RATE);
                    worst = worst.max(check(model, cutoff, resonance, 0.0, &curve));
                }
                // The SEM-like filter at either end of its morph, and its
                // notch halfway, at half the level.
                let q = sem_q(resonance);
                let curve = SvfResponse::new(SvfMode::Lowpass, cutoff, q, RATE);
                worst = worst.max(check(FilterModel::SemNotch, cutoff, resonance, 0.0, &curve));
                let curve = SvfResponse::new(SvfMode::Highpass, cutoff, q, RATE);
                worst = worst.max(check(FilterModel::SemBand, cutoff, resonance, 1.0, &curve));
                let curve = SvfResponse::new(SvfMode::Bandpass, cutoff, q, RATE);
                worst = worst.max(check(FilterModel::SemBand, cutoff, resonance, 0.5, &curve));
                let curve = Scaled(SvfResponse::new(SvfMode::Notch, cutoff, q, RATE), 0.5);
                worst = worst.max(check(FilterModel::SemNotch, cutoff, resonance, 0.5, &curve));
                // The MS-20-like filter is the state-variable filter of
                // the same Q.
                let q = 1.0 / (2.0 - sk_feedback(resonance));
                let curve = SvfResponse::new(SvfMode::Lowpass, cutoff, q, RATE);
                worst = worst.max(check(FilterModel::Ms20Lp, cutoff, resonance, 0.0, &curve));
                let curve = SvfResponse::new(SvfMode::Highpass, cutoff, q, RATE);
                worst = worst.max(check(FilterModel::Ms20Hp, cutoff, resonance, 0.0, &curve));
            }
        }
        println!("worst relative error {worst:e}");
    }

    #[test]
    fn the_combs_ring_at_the_harmonics_of_their_cutoff() {
        for (model, offset) in [(FilterModel::CombPlus, 0.0), (FilterModel::CombMinus, 0.5)] {
            for damping in [0.0, 0.6] {
                let cutoff = 220.0;
                let response = impulse_response(model, cutoff, 0.9, damping, 1 << 16);
                for harmonic in 0..4 {
                    let peak = (harmonic as f64 + offset).max(0.5) * cutoff;
                    if harmonic == 0 && offset == 0.0 {
                        continue;
                    }
                    // The response is highest within a few cents of the
                    // resonance.
                    let at = |f: f64| gain_at(&response, f, RATE);
                    let best = (-40..=40)
                        .map(|cents| peak * (f64::from(cents) / 1_200.0).exp2())
                        .max_by(|a, b| at(*a).total_cmp(&at(*b)))
                        .unwrap();
                    let off = 1_200.0 * (best / peak).log2();
                    assert!(
                        off.abs() <= 3.0,
                        "{model:?} {damping} {harmonic}: {off} cents"
                    );
                    // And well above what lies between.
                    let between = at(peak + 0.5 * cutoff);
                    assert!(at(best) > 4.0 * between, "{model:?} {harmonic}");
                }
            }
        }
    }

    #[test]
    fn the_formants_peak_where_the_vowels_have_them() {
        for (vowel, formants) in VOWELS.iter().enumerate() {
            let morph = vowel as f64 / (VOWELS.len() - 1) as f64;
            let response = impulse_response(FilterModel::Formant, 1_000.0, 0.5, morph, 1 << 15);
            for &(centre, _, _) in formants {
                let at = |f: f64| gain_at(&response, f, RATE);
                // A peak: higher than half an octave either side.
                let here = at(centre);
                assert!(
                    here > 1.5 * at(centre * 0.7) || here > 1.5 * at(centre * 1.41),
                    "{vowel} {centre}"
                );
            }
        }
    }

    #[test]
    fn the_cheap_tanh_is_close_monotonic_and_bounded() {
        let mut worst_near = 0.0f64;
        let mut worst = 0.0f64;
        let mut last = -1.0;
        for n in -100_000..=100_000 {
            let x = f64::from(n) * 1e-4;
            let y = fast_tanh(x);
            let error = (y - x.tanh()).abs();
            if x.abs() <= 3.0 {
                worst_near = worst_near.max(error);
            }
            worst = worst.max(error);
            assert!(y >= last && y.abs() <= 1.0, "{x}");
            last = y;
        }
        assert!(worst_near < 2e-6 && worst < 1.2e-4, "{worst_near} {worst}");
        // Near zero it is tanh to the seventh power.
        for x in [1e-7, 1e-3, 0.05] {
            assert!((fast_tanh(x) - x.tanh()).abs() < 1e-9 * x, "{x}");
        }
        assert_eq!(fast_tanh(f64::INFINITY), 1.0);
        assert_eq!(fast_tanh(-1e300), -1.0);
        assert!(ladder_loop(f64::NAN, 2.0).abs() <= 1.0);
    }

    #[test]
    fn loud_and_broken_inputs_stay_bounded() {
        for model in FilterModel::ALL {
            let mut slot = FilterSlot::new();
            slot.prepare(RATE);
            slot.set_model(model, true);
            let c = Coefficients::new(model, 12_000.0, 1.0, 36.0, 0.3, RATE);
            let mut peak = 0.0f64;
            for n in 0..20_000 {
                let x = match n % 7 {
                    0 => 1e9,
                    1 => f64::NAN,
                    2 => -1e300,
                    3 => f64::INFINITY,
                    _ => {
                        if n % 2 == 0 {
                            3.0
                        } else {
                            -3.0
                        }
                    }
                };
                let x = crate::numeric::limit(x, -64.0, 64.0);
                let y = slot.process(0, x, &c, &c, true, true);
                assert!(y.is_finite(), "{model:?} at {n}");
                peak = peak.max(y.abs());
            }
            assert!(peak < 1e3, "{model:?}: {peak}");
        }
        assert!(loop_solution(f64::NAN, 0.5).abs() <= 1.0);
        assert_eq!(loop_solution(1e300, 0.5), 1.0);
    }
}
