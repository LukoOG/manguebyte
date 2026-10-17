//! The poly synth's oscillators: band-limited shapes with up to sixteen
//! unison voices, hard sync and phase modulation.
//!
//! # Band-limiting
//!
//! A sawtooth, a pulse and a synced oscillator jump; a triangle and a
//! synced sine bend. Drawn naively, every jump puts harmonics up to
//! infinity into the signal and those above half the sample rate fold
//! back. Here each jump is replaced by a band-limited step and each bend
//! by a band-limited ramp, the "polyBLEP" and "polyBLAMP" method of
//! V. Välimäki, J. Pekonen and J. Nam, "Perceptually informed synthesis
//! of bandlimited classical waveforms using integrated polynomial
//! interpolation", JASA 2012, at its four-point order: the step is the
//! integral of the cubic B-spline, which reaches two samples either side
//! of the jump, and the ramp is the integral of that. Their residuals
//! (what is added to the naive waveform) are piecewise polynomials of the
//! fraction of a sample at which the jump falls, worked out in [`blep`]
//! and [`blamp`].
//!
//! The residual reaches two samples before the jump, so an oscillator puts
//! out every sample two samples late, which lets a jump found only when
//! the phase passes it (a master's reset, a modulated phase) still be
//! corrected on both sides. The B-spline's spectrum is `sinc⁴`, so the
//! images that fold back are attenuated by its fourth power; and the voice
//! runs at twice the sample rate, so only what folds below a quarter of
//! the internal rate is heard at all.
//!
//! # Unison
//!
//! Each unison voice has its own phase, detune and place in the stereo
//! field. The voices of an oscillator are added into one stereo signal,
//! scaled by one over the square root of their number so that, their
//! phases being unrelated, the level stays the same however many there
//! are. Their gains glide, so voices are added and taken away without a
//! click.

use core::f64::consts::{FRAC_PI_4, SQRT_2, TAU};

use super::super::common::Noise;
use super::params::MAX_UNISON;
use super::tables::Table;

/// The residual of a band-limited unit step at fraction `d` (0 to 1) of
/// the way from sample `n - 1` to sample `n`, for samples `n - 2`, `n - 1`,
/// `n` and `n + 1`.
///
/// With `S(x)` the integral of the cubic B-spline (support -2 to 2) and
/// `H(x)` the unit step, the residual is `S(x) - H(x)` at `x` = the
/// distance of each sample from the jump: `(2 + x)⁴/24` from -2 to -1,
/// `1/2 + 2x/3 - x³/3 - x⁴/8` from -1 to 0, its mirror image negated from
/// 0 to 2.
#[inline]
pub(super) fn blep(d: f64) -> [f64; 4] {
    let e = 1.0 - d;
    let d2 = d * d;
    let e2 = e * e;
    [
        e2 * e2 / 24.0,
        0.5 - d * (2.0 / 3.0) + d2 * d / 3.0 - d2 * d2 / 8.0,
        -0.5 + e * (2.0 / 3.0) - e2 * e / 3.0 + e2 * e2 / 8.0,
        -d2 * d2 / 24.0,
    ]
}

/// The residual of a band-limited unit corner (a change of slope of one
/// per sample) at fraction `d`, for the same four samples: the integral of
/// [`blep`]'s, `(2 + x)⁵/120` from -2 to -1 and `7/30 + x/2 + x²/3 - x⁴/12
/// - x⁵/40` from -1 to 0, mirrored (not negated) from 0 to 2.
#[inline]
pub(super) fn blamp(d: f64) -> [f64; 4] {
    let e = 1.0 - d;
    let middle = |x: f64| {
        let x2 = x * x;
        7.0 / 30.0 - 0.5 * x + x2 / 3.0 - x2 * x2 / 12.0 + x2 * x2 * x / 40.0
    };
    let e2 = e * e;
    let d2 = d * d;
    [
        e2 * e2 * e / 120.0,
        middle(d),
        middle(e),
        d2 * d2 * d / 120.0,
    ]
}

/// The output of an oscillator, two samples late, with room for the
/// corrections of the jumps around each sample: samples `n - 2` to `n + 1`
/// for each channel.
#[derive(Debug, Clone, Copy, Default)]
struct Line {
    left: [f64; 4],
    right: [f64; 4],
    base: usize,
}

impl Line {
    /// Adds to sample `n`.
    #[inline]
    fn add_now(&mut self, left: f64, right: f64) {
        let index = (self.base + 2) & 3;
        self.left[index] += left;
        self.right[index] += right;
    }

    /// Adds `kernel` times the gains to samples `n - 2` to `n + 1`.
    #[inline]
    fn add_kernel(&mut self, kernel: [f64; 4], left: f64, right: f64) {
        for (offset, &value) in kernel.iter().enumerate() {
            let index = (self.base + offset) & 3;
            self.left[index] += value * left;
            self.right[index] += value * right;
        }
    }

    /// Takes sample `n - 2`, which nothing can change any more, and moves
    /// on by a sample.
    #[inline]
    fn emit(&mut self) -> (f64, f64) {
        let index = self.base;
        let out = (self.left[index], self.right[index]);
        self.left[index] = 0.0;
        self.right[index] = 0.0;
        self.base = (index + 1) & 3;
        out
    }
}

/// A shape as the oscillator draws it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Shape {
    Sine,
    Triangle,
    Saw,
    Pulse,
    /// A wavetable, by its index in the bank.
    Table(usize),
}

/// What an oscillator needs from its tables and settings to draw a
/// sample.
#[derive(Clone, Copy)]
pub(super) struct Drawing<'a> {
    /// The width of a pulse, 0.01 to 0.99.
    pub(super) width: f64,
    /// The position in a wavetable, 0 to 1.
    pub(super) position: f64,
    /// The mip-map level to read.
    pub(super) level: usize,
    pub(super) bank: &'a [Table],
}

impl Drawing<'_> {
    /// The naive waveform at `phase`, 0 up to 1.
    #[inline]
    fn value(&self, shape: Shape, phase: f64) -> f64 {
        match shape {
            Shape::Sine => sine(phase),
            Shape::Triangle => {
                if phase < 0.5 {
                    4.0 * phase - 1.0
                } else {
                    3.0 - 4.0 * phase
                }
            }
            Shape::Saw => 2.0 * phase - 1.0,
            Shape::Pulse => {
                // Without its mean, so that a moving width does not move
                // the level.
                let high = if phase < self.width { 1.0 } else { -1.0 };
                high - (2.0 * self.width - 1.0)
            }
            Shape::Table(index) => self.bank[index].read(self.level, self.position, phase),
        }
    }

    /// The slope of the waveform at `phase`, per cycle.
    #[inline]
    fn slope(&self, shape: Shape, phase: f64) -> f64 {
        match shape {
            Shape::Sine => TAU * sine(phase + 0.25),
            Shape::Triangle => {
                if phase < 0.5 {
                    4.0
                } else {
                    -4.0
                }
            }
            Shape::Saw => 2.0,
            Shape::Pulse => 0.0,
            Shape::Table(_) => {
                const STEP: f64 = 1e-3;
                let before = wrap(phase - STEP);
                let after = wrap(phase + STEP);
                (self.value(shape, after) - self.value(shape, before)) / (2.0 * STEP)
            }
        }
    }
}

/// A phase brought into 0 up to 1.
#[inline]
pub(super) fn wrap(phase: f64) -> f64 {
    let wrapped = phase - phase.floor();
    if wrapped < 1.0 { wrapped } else { 0.0 }
}

/// `sin(2π·phase)`: the phase folded to the quarter cycle either side of
/// zero, where the Taylor series of the sine to its seventh term (`x¹³`)
/// is within 7·10⁻¹⁰ of it, so that what it adds is 180 dB down. Several
/// times cheaper than the library's sine, which the oscillators would
/// call for every unison voice of every sample.
#[inline]
pub(super) fn sine(phase: f64) -> f64 {
    let mut t = phase - phase.round();
    if t > 0.25 {
        t = 0.5 - t;
    } else if t < -0.25 {
        t = -0.5 - t;
    }
    let x = TAU * t;
    let x2 = x * x;
    x * (1.0
        + x2 * (-1.0 / 6.0
            + x2 * (1.0 / 120.0
                + x2 * (-1.0 / 5_040.0
                    + x2 * (1.0 / 362_880.0 + x2 * (-1.0 / 39_916_800.0 + x2 / 6_227_020_800.0))))))
}

/// Where unison voice `index` of `count` sits, from -1 to 1, in pitch
/// order: evenly spread, or 0 for a single voice.
#[inline]
fn spot(index: usize, count: usize) -> f64 {
    if count < 2 {
        0.0
    } else {
        2.0 * index as f64 / (count - 1) as f64 - 1.0
    }
}

/// How far unison voice `index` of `count` is detuned, as a share of the
/// outermost voices' detune: `|x|^1.5` with the sign of `x`, denser in the
/// middle, close to the spread A. Szabo measured on the classic supersaw
/// ("How to emulate the super saw", 2010).
#[inline]
pub(super) fn detune_share(index: usize, count: usize) -> f64 {
    let x = spot(index, count);
    x.signum() * x.abs().powf(1.5)
}

/// Where unison voice `index` of `count` goes in the stereo field, -1 to
/// 1: voices next to each other in pitch go to opposite sides, the even
/// ones from the left inwards and the odd ones from the right.
#[inline]
pub(super) fn pan_share(index: usize, count: usize) -> f64 {
    if count < 2 {
        return 0.0;
    }
    let rank = if index % 2 == 0 {
        index / 2
    } else {
        count - 1 - index / 2
    };
    spot(rank, count)
}

/// The gains of a place in the stereo field, -1 to 1, at equal power and
/// with both at 1 in the middle.
#[inline]
pub(super) fn pan_gains(pan: f64) -> (f64, f64) {
    let angle = (pan.clamp(-1.0, 1.0) + 1.0) * FRAC_PI_4;
    (SQRT_2 * angle.cos(), SQRT_2 * angle.sin())
}

/// A jump or a bend in the waveform, found along the path of a phase.
#[derive(Debug, Clone, Copy)]
enum Event {
    /// The waveform jumps by this much.
    Step(f64),
    /// Its slope, per sample, changes by this much.
    Corner(f64),
}

/// One oscillator: up to [`MAX_UNISON`] voices and their output.
#[derive(Debug, Clone)]
pub(super) struct Osc {
    /// Each voice's phase accumulator, 0 up to 1.
    phase: [f64; MAX_UNISON],
    /// Each voice's phase offset from phase modulation, in cycles, at the
    /// last sample.
    pm: [f64; MAX_UNISON],
    /// Each voice's naive output at the last sample: what it gives an
    /// oscillator it modulates.
    pub(super) value: [f64; MAX_UNISON],
    /// For each voice, the fraction of the last sample at which its phase
    /// started a cycle, or 0 if it did not: what a synced oscillator
    /// follows.
    pub(super) wrapped: [f64; MAX_UNISON],
    /// Each voice's frequency as a multiple of the oscillator's.
    ratio: [f64; MAX_UNISON],
    /// Each voice's left and right gain, and where they are heading.
    gain: [(f64, f64); MAX_UNISON],
    target: [(f64, f64); MAX_UNISON],
    /// How many voices are wanted, and how many still sound (those fading
    /// out included).
    voices: usize,
    running: usize,
    /// The detune, pan and spread the ratios and gains were made for.
    made_for: (f64, f64, f64),
    line: Line,
    /// The shape, and the one it is fading from, by `blend` (1: only the
    /// new one).
    shape: Shape,
    previous: Shape,
    blend: f64,
    /// Whether the gains have arrived and no shape is fading.
    settled: bool,
    /// The last sample put out, left and right, before the level.
    pub(super) out: (f64, f64),
}

impl Osc {
    pub(super) fn new() -> Self {
        Self {
            phase: [0.0; MAX_UNISON],
            pm: [0.0; MAX_UNISON],
            value: [0.0; MAX_UNISON],
            wrapped: [0.0; MAX_UNISON],
            ratio: [1.0; MAX_UNISON],
            gain: [(0.0, 0.0); MAX_UNISON],
            target: [(0.0, 0.0); MAX_UNISON],
            voices: 1,
            running: 0,
            made_for: (f64::NAN, f64::NAN, f64::NAN),
            line: Line::default(),
            shape: Shape::Saw,
            previous: Shape::Saw,
            blend: 1.0,
            settled: false,
            out: (0.0, 0.0),
        }
    }

    /// How many voices the oscillator plays.
    pub(super) fn voices(&self) -> usize {
        self.voices
    }

    /// Back to the state of a new oscillator.
    pub(super) fn silence(&mut self) {
        *self = Self::new();
    }

    /// Starts a note: every voice at `phase`, moved by up to
    /// `random_phase` of a cycle at random, and the gains where they are
    /// heading. The voices not playing yet start anywhere, so that if
    /// more voices are asked for during the note they join unrelated to
    /// the others.
    pub(super) fn strike(
        &mut self,
        shape: Shape,
        phase: f64,
        random_phase: f64,
        noise: &mut Noise,
    ) {
        let voices = self.voices;
        for (index, slot) in self.phase.iter_mut().enumerate() {
            let spread = if index < voices { random_phase } else { 1.0 };
            let offset = spread * noise.unit();
            *slot = wrap(phase + offset);
            self.pm[index] = 0.0;
            self.value[index] = 0.0;
            self.wrapped[index] = 0.0;
        }
        self.shape = shape;
        self.previous = shape;
        self.blend = 1.0;
        self.settled = true;
        self.gain = self.target;
        self.running = self.voices;
    }

    /// Takes the shape, fading to it from the one before over the time
    /// that `step` (the share per sample) gives.
    pub(super) fn set_shape(&mut self, shape: Shape) {
        if shape != self.shape {
            self.previous = self.shape;
            self.shape = shape;
            self.blend = 0.0;
            self.settled = false;
        }
    }

    /// Takes how many voices to play and how they are detuned and spread,
    /// working out their ratios and gains only when something changed.
    pub(super) fn arrange(&mut self, voices: usize, detune: f64, pan: f64, spread: f64) {
        let voices = voices.clamp(1, MAX_UNISON);
        let recount = voices != self.voices;
        if recount || detune != self.made_for.0 {
            // A detune of at most 100 cents is a ratio within 6% of 1, whose
            // exponential four terms of its series give to 10⁻⁸: a
            // hundred-thousandth of a cent.
            let cents = super::params::MAX_DETUNE_CENTS * detune * detune;
            let scale = cents * (core::f64::consts::LN_2 / 1_200.0);
            for (index, ratio) in self.ratio.iter_mut().enumerate().take(voices) {
                let x = scale * detune_share(index, voices);
                *ratio = 1.0 + x * (1.0 + x * (0.5 + x * (1.0 / 6.0 + x / 24.0)));
            }
        }
        if recount || (pan, spread) != (self.made_for.1, self.made_for.2) {
            let norm = 1.0 / (voices as f64).sqrt();
            for (index, target) in self.target.iter_mut().enumerate() {
                *target = if index < voices {
                    let (left, right) = pan_gains(pan + spread * pan_share(index, voices));
                    (left * norm, right * norm)
                } else {
                    (0.0, 0.0)
                };
            }
            self.settled = false;
        }
        self.voices = voices;
        self.running = self.running.max(voices);
        self.made_for = (detune, pan, spread);
    }

    /// Moves the gains and the shape's fade on by one output sample:
    /// `follow` of the way to the gains wanted, `fade` of the way to the
    /// new shape.
    pub(super) fn glide(&mut self, follow: f64, fade: f64) {
        if self.settled {
            return;
        }
        let mut running = self.voices;
        let mut arrived = true;
        for (index, (gain, target)) in self.gain.iter_mut().zip(&self.target).enumerate() {
            let (left, right) = (target.0 - gain.0, target.1 - gain.1);
            if left.abs() + right.abs() < 1e-9 {
                *gain = *target;
            } else {
                gain.0 += left * follow;
                gain.1 += right * follow;
                arrived = false;
            }
            if index >= self.voices {
                if gain.0.abs() + gain.1.abs() < 1e-6 {
                    *gain = (0.0, 0.0);
                } else {
                    running = index + 1;
                }
            }
        }
        self.running = running;
        if self.blend < 1.0 {
            self.blend = (self.blend + fade).min(1.0);
            arrived = false;
        }
        self.settled = arrived;
    }

    /// One sample at `increment` cycles per sample.
    ///
    /// `pm_depth` is the phase modulation in cycles per unit of
    /// `modulator`, whose first `modulator_voices` values modulate this
    /// oscillator's voices in turn. A synced oscillator is given its
    /// master's [`wrapped`](Self::wrapped) and number of voices. Returns
    /// the sample from two samples ago, left and right.
    #[inline]
    pub(super) fn tick(
        &mut self,
        increment: f64,
        drawing: &Drawing<'_>,
        pm_depth: f64,
        modulator: &[f64; MAX_UNISON],
        modulator_voices: usize,
        master: Option<(&[f64; MAX_UNISON], usize)>,
    ) -> (f64, f64) {
        let shapes = [(self.shape, self.blend), (self.previous, 1.0 - self.blend)];
        let modulator_voices = modulator_voices.clamp(1, MAX_UNISON);
        // A voice neither modulated, synced nor changing shape takes the
        // short way.
        let plain = pm_depth == 0.0 && master.is_none() && self.blend >= 1.0;
        for index in 0..self.running {
            let (left, right) = self.gain[index];
            let dt = (increment * self.ratio[index]).clamp(0.0, 0.45);
            if plain && self.pm[index] == 0.0 {
                let value = self.plain(index, dt, drawing, left, right);
                self.value[index] = value;
                self.line.add_now(value * left, value * right);
                continue;
            }
            let pm = if pm_depth != 0.0 {
                pm_depth * modulator[index % modulator_voices]
            } else {
                0.0
            };
            let reset = match master {
                Some((wrapped, count)) => wrapped[index % count.clamp(1, MAX_UNISON)],
                None => 0.0,
            };
            let value = self.voice(index, dt, pm, reset, &shapes, drawing, left, right);
            self.value[index] = value;
            self.line.add_now(value * left, value * right);
        }
        self.out = self.line.emit();
        self.out
    }

    /// [`voice`](Self::voice) for a voice of one shape, with no phase
    /// modulation and no sync: the same corrections, found directly.
    #[inline]
    fn plain(
        &mut self,
        index: usize,
        dt: f64,
        drawing: &Drawing<'_>,
        left: f64,
        right: f64,
    ) -> f64 {
        let phase = self.phase[index];
        let unwrapped = phase + dt;
        let wrapped = unwrapped >= 1.0;
        let next = if wrapped { unwrapped - 1.0 } else { unwrapped };
        let at = if wrapped {
            ((1.0 - phase) / dt).clamp(1e-12, 1.0)
        } else {
            0.0
        };
        self.wrapped[index] = at;
        self.phase[index] = next;
        let line = &mut self.line;
        // The time, within the sample, at which the phase passes `point`.
        let when = |point: f64| ((point - phase) / dt).clamp(1e-12, 1.0);
        match self.shape {
            Shape::Saw => {
                if wrapped {
                    line.add_kernel(blep(at), -2.0 * left, -2.0 * right);
                }
                2.0 * next - 1.0
            }
            Shape::Pulse => {
                let width = drawing.width;
                if wrapped {
                    line.add_kernel(blep(at), 2.0 * left, 2.0 * right);
                }
                let edge = if phase < width && unwrapped >= width {
                    Some(width)
                } else if unwrapped >= 1.0 + width {
                    Some(1.0 + width)
                } else {
                    None
                };
                if let Some(point) = edge {
                    line.add_kernel(blep(when(point)), -2.0 * left, -2.0 * right);
                }
                let high = if next < width { 1.0 } else { -1.0 };
                high - (2.0 * width - 1.0)
            }
            Shape::Triangle => {
                let change = 8.0 * dt;
                if phase < 0.5 && unwrapped >= 0.5 {
                    line.add_kernel(blamp(when(0.5)), -change * left, -change * right);
                }
                if wrapped {
                    line.add_kernel(blamp(at), change * left, change * right);
                }
                if unwrapped >= 1.5 {
                    line.add_kernel(blamp(when(1.5)), -change * left, -change * right);
                }
                drawing.value(Shape::Triangle, next)
            }
            shape => drawing.value(shape, next),
        }
    }

    /// Moves voice `index` on by a sample, adds the corrections of every
    /// jump and bend on the way, and returns its naive value at the new
    /// phase.
    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn voice(
        &mut self,
        index: usize,
        dt: f64,
        pm: f64,
        reset: f64,
        shapes: &[(Shape, f64); 2],
        drawing: &Drawing<'_>,
        left: f64,
        right: f64,
    ) -> f64 {
        let old_pm = self.pm[index];
        let start = wrap(self.phase[index] + old_pm);
        // How far the phase that is read moves in a sample.
        let rate = dt + (pm - old_pm);
        let line = &mut self.line;
        let mut correct = |at: f64, event: Event| match event {
            Event::Step(height) => line.add_kernel(blep(at), height * left, height * right),
            Event::Corner(change) => line.add_kernel(blamp(at), change * left, change * right),
        };
        if reset > 0.0 {
            // The master starts its cycle at `reset`: so does this voice.
            let before = start + rate * reset;
            let pm_then = old_pm + reset * (pm - old_pm);
            let after = wrap(pm_then);
            let (mut height, mut bend) = (0.0, 0.0);
            for &(shape, weight) in shapes {
                if weight > 0.0 {
                    path(
                        shape,
                        drawing.width,
                        start,
                        rate,
                        0.0,
                        reset,
                        weight,
                        &mut correct,
                    );
                    let from = wrap(before);
                    height += weight * (drawing.value(shape, after) - drawing.value(shape, from));
                    bend +=
                        weight * rate * (drawing.slope(shape, after) - drawing.slope(shape, from));
                    path(
                        shape,
                        drawing.width,
                        after,
                        rate,
                        reset,
                        1.0,
                        weight,
                        &mut correct,
                    );
                }
            }
            if height != 0.0 {
                correct(reset, Event::Step(height));
            }
            if bend != 0.0 {
                correct(reset, Event::Corner(bend));
            }
            self.phase[index] = wrap((1.0 - reset) * dt);
            self.wrapped[index] = 0.0;
        } else {
            for &(shape, weight) in shapes {
                if weight > 0.0 {
                    path(
                        shape,
                        drawing.width,
                        start,
                        rate,
                        0.0,
                        1.0,
                        weight,
                        &mut correct,
                    );
                }
            }
            let next = self.phase[index] + dt;
            if next >= 1.0 {
                self.wrapped[index] = ((1.0 - self.phase[index]) / dt).clamp(1e-12, 1.0);
                self.phase[index] = wrap(next - 1.0);
            } else {
                self.wrapped[index] = 0.0;
                self.phase[index] = next;
            }
        }
        self.pm[index] = pm;
        let now = wrap(self.phase[index] + pm);
        let mut value = 0.0;
        for &(shape, weight) in shapes {
            if weight > 0.0 {
                value += weight * drawing.value(shape, now);
            }
        }
        value
    }
}

/// Finds the jumps and bends of `shape` while the phase read moves from
/// `start` at `rate` per sample from time `t0` to `t1` (fractions of the
/// sample), and hands each to `correct` with its time, scaled by
/// `weight`. A path longer than half a cycle is a phase that moves too
/// fast to be band-limited, and is left alone.
#[inline]
#[allow(clippy::too_many_arguments)]
fn path(
    shape: Shape,
    width: f64,
    start: f64,
    rate: f64,
    t0: f64,
    t1: f64,
    weight: f64,
    correct: &mut impl FnMut(f64, Event),
) {
    let length = rate * (t1 - t0);
    if length == 0.0 || length.abs() > 0.5 {
        return;
    }
    let end = start + length;
    // The time at which the phase passes `point`, if it does.
    let crossing = |point: f64| -> Option<f64> {
        let passes = if length > 0.0 {
            start < point && point <= end
        } else {
            end < point && point <= start
        };
        passes.then(|| (t0 + (point - start) / rate).clamp(1e-12, 1.0))
    };
    let forward = length > 0.0;
    let direction = if forward { 1.0 } else { -1.0 };
    match shape {
        Shape::Saw => {
            let wrap_point = if forward { 1.0 } else { 0.0 };
            if let Some(at) = crossing(wrap_point) {
                correct(at, Event::Step(-2.0 * direction * weight));
            }
        }
        Shape::Pulse => {
            let wrap_point = if forward { 1.0 } else { 0.0 };
            if let Some(at) = crossing(wrap_point) {
                correct(at, Event::Step(2.0 * direction * weight));
            }
            for edge in [width, width + 1.0, width - 1.0] {
                if let Some(at) = crossing(edge) {
                    correct(at, Event::Step(-2.0 * direction * weight));
                }
            }
        }
        Shape::Triangle => {
            let change = 8.0 * rate.abs() * weight;
            for low in [0.0, 1.0] {
                if let Some(at) = crossing(low) {
                    correct(at, Event::Corner(change));
                }
            }
            for high in [0.5, 1.5, -0.5] {
                if let Some(at) = crossing(high) {
                    correct(at, Event::Corner(-change));
                }
            }
        }
        Shape::Sine | Shape::Table(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_residuals_are_those_of_the_integrated_b_spline() {
        // The step: continuous across the jump, summing with the naive
        // step to the B-spline's integral, which at the four samples
        // around a jump halfway is 1/24·(1/2)⁴ ... and is symmetric.
        for d in [1e-9, 0.1, 0.25, 0.5, 0.77, 1.0] {
            let r = blep(d);
            let mirrored = blep(1.0 - d);
            for j in 0..4 {
                assert!((r[j] + mirrored[3 - j]).abs() < 1e-12, "{d} {j}");
            }
            let q = blamp(d);
            let q_mirrored = blamp(1.0 - d);
            for j in 0..4 {
                assert!((q[j] - q_mirrored[3 - j]).abs() < 1e-12, "{d} {j}");
            }
        }
        // At d = 1 the jump is on sample n: halfway up there.
        let r = blep(1.0);
        assert!((r[2] + 0.5).abs() < 1e-15 && (r[1] - 1.0 / 24.0).abs() < 1e-15);
        assert!(r[0].abs() < 1e-15 && (r[3] + 1.0 / 24.0).abs() < 1e-15);
        // The corrected step, sampled finely, is the integral of the
        // spline: its numerical derivative has the spline's area of 1.
        let q = blamp(1.0);
        assert!((q[2] - 7.0 / 30.0).abs() < 1e-15 && (q[1] - 1.0 / 120.0).abs() < 1e-15);
    }

    #[test]
    fn the_cheap_sine_is_the_sine() {
        let mut worst = 0.0f64;
        for n in -20_000..=20_000 {
            let phase = f64::from(n) * 1.37e-4;
            worst = worst.max((sine(phase) - (TAU * phase).sin()).abs());
        }
        assert!(worst < 1e-9, "{worst}");
        assert_eq!(sine(0.0), 0.0);
        assert!((sine(0.25) - 1.0).abs() < 1e-9 && (sine(0.75) + 1.0).abs() < 1e-9);
    }

    #[test]
    fn unison_voices_spread_evenly_and_alternate_sides() {
        assert_eq!(detune_share(0, 1), 0.0);
        assert_eq!(detune_share(0, 7), -1.0);
        assert_eq!(detune_share(6, 7), 1.0);
        assert_eq!(detune_share(3, 7), 0.0);
        let inner = detune_share(4, 7);
        assert!((inner - (1.0f64 / 3.0).powf(1.5)).abs() < 1e-12);
        for count in 2..=MAX_UNISON {
            let mut pans: Vec<f64> = (0..count).map(|i| pan_share(i, count)).collect();
            // Neighbours in pitch are on opposite sides, or one is in the
            // middle.
            for i in 0..count - 1 {
                let (a, b) = (pan_share(i, count), pan_share(i + 1, count));
                assert!(a * b <= 1e-12, "{count}: {i}");
            }
            pans.sort_by(f64::total_cmp);
            for (rank, pan) in pans.iter().enumerate() {
                assert!((pan - spot(rank, count)).abs() < 1e-12);
            }
        }
        let (left, right) = pan_gains(0.0);
        assert!((left - 1.0).abs() < 1e-12 && (right - 1.0).abs() < 1e-12);
        let (left, right) = pan_gains(-1.0);
        assert!((left - SQRT_2).abs() < 1e-12 && right.abs() < 1e-12);
        let (left, right) = pan_gains(0.3);
        assert!((left * left + right * right - 2.0).abs() < 1e-12);
    }
}
