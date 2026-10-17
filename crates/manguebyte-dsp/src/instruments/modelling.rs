//! What the physical models of acoustic instruments share: delays read
//! in tune, loss filters designed from decay times, vibrato, glides, and
//! the dynamics a player controls while a note sounds.

use core::f64::consts::TAU;

use manguebyte_midi::MidiMessage;

use super::common::{Noise, flush, setting};
use crate::event::Expression;

/// The phase delay, in samples, at `w` radians per sample, of
/// [`DelayLine::read_cubic`] asked for `delay` samples.
///
/// The cubic (third-order Lagrange) interpolator is exact at low
/// frequencies and drifts a little towards the top of the spectrum; this
/// is how far, so that a waveguide can ask for slightly more or less and be
/// in tune at its fundamental.
pub(crate) fn cubic_phase_delay(delay: f64, w: f64) -> f64 {
    let w = w.max(1e-9);
    if delay < 2.0 {
        // Read along a straight line between the two newest samples.
        let f = (delay - 1.0).clamp(0.0, 1.0);
        let re = (1.0 - f) * w.cos() + f * (2.0 * w).cos();
        let im = -(1.0 - f) * w.sin() - f * (2.0 * w).sin();
        return -im.atan2(re) / w;
    }
    let newest = delay as usize - 1;
    let t = delay - newest as f64;
    let (a, b, c, d) = (t, t - 1.0, t - 2.0, t - 3.0);
    let weights = [
        -b * c * d / 6.0,
        a * c * d / 2.0,
        -a * b * d / 2.0,
        a * b * c / 6.0,
    ];
    let (mut re, mut im) = (0.0, 0.0);
    for (offset, weight) in weights.into_iter().enumerate() {
        let phase = w * offset as f64;
        re += weight * phase.cos();
        im -= weight * phase.sin();
    }
    newest as f64 - im.atan2(re) / w
}

/// The delay to ask [`DelayLine::read_cubic`] for so that its phase delay
/// at `w` is `target` samples: three steps of a fixed-point search, which
/// leave an error below a hundred-thousandth of a sample up to 0.15 of the
/// sample rate.
pub(crate) fn cubic_delay_for(target: f64, w: f64) -> f64 {
    let mut delay = target;
    for _ in 0..3 {
        delay += target - cubic_phase_delay(delay, w);
    }
    if delay.is_finite() { delay } else { target }
}

/// A one-pole low-pass `b0 / (1 + a z⁻¹)`, the loss of a waveguide: the
/// gain of one trip round the instrument, falling with frequency.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Loss {
    pub(crate) b0: f64,
    pub(crate) a: f64,
}

impl Loss {
    /// No loss and no delay.
    pub(crate) const NONE: Self = Self { b0: 1.0, a: 0.0 };

    /// The filter whose gain is `g0` at `w0` and `gh` at `wh` radians per
    /// sample, for `gh <= g0 < 1`.
    ///
    /// The ratio of the gains fixes the pole: the condition is a quadratic
    /// in `a` whose roots multiply to 1, and the root inside the unit
    /// circle is the filter (the design of V. Välimäki, J. Huopaniemi, M.
    /// Karjalainen and Z. Jánosy, "Physical modeling of plucked string
    /// instruments with application to real-time sound synthesis", JAES
    /// 1996). A ratio no one-pole filter can reach gets the steepest one
    /// allowed; `b0` then gives the gain at `w0`.
    pub(crate) fn design(g0: f64, gh: f64, w0: f64, wh: f64) -> Self {
        let g0 = setting(g0, 0.0, 0.999_99, 0.99);
        let ratio = if g0 > 0.0 { gh / g0 } else { 1.0 };
        let a = if ratio.is_nan() || ratio >= 0.999_999 || wh <= 1.01 * w0 {
            0.0
        } else {
            let r2 = ratio.max(0.0).powi(2);
            let (c0, ch) = (w0.cos(), wh.cos());
            let quadratic = r2 - 1.0;
            let linear = 2.0 * (r2 * ch - c0);
            let discriminant = (linear * linear - 4.0 * quadratic * quadratic)
                .max(0.0)
                .sqrt();
            let roots = [
                (-linear + discriminant) / (2.0 * quadratic),
                (-linear - discriminant) / (2.0 * quadratic),
            ];
            let inside = if roots[0].abs() <= roots[1].abs() {
                roots[0]
            } else {
                roots[1]
            };
            if inside.is_finite() {
                inside.clamp(-0.98, 0.0)
            } else {
                0.0
            }
        };
        let b0 = g0 * (1.0 + 2.0 * a * w0.cos() + a * a).sqrt();
        Self { b0, a }
    }

    /// The gain at `w` radians per sample.
    pub(crate) fn gain(&self, w: f64) -> f64 {
        self.b0 / (1.0 + 2.0 * self.a * w.cos() + self.a * self.a).sqrt()
    }

    /// The phase delay, in samples, at `w` radians per sample.
    pub(crate) fn delay(&self, w: f64) -> f64 {
        let w = w.max(1e-9);
        (-self.a * w.sin()).atan2(1.0 + self.a * w.cos()) / w
    }

    /// One step, with `memory` the last output.
    #[inline]
    pub(crate) fn step(&self, memory: &mut f64, x: f64) -> f64 {
        *memory = flush(self.b0 * x - self.a * *memory);
        *memory
    }
}

/// The gain per trip of `period` samples that makes a component of the
/// waveguide fall by 60 dB in `seconds`, at `rate`.
pub(crate) fn trip_gain(seconds: f64, period: f64, rate: f64) -> f64 {
    if seconds > 0.0 {
        10f64.powf(-3.0 * period / (seconds * rate))
    } else {
        0.0
    }
}

/// A first-order high-pass `(1 - z⁻¹) / (1 - R z⁻¹)` that keeps the
/// constant part of a signal out of a loop.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct DcBlock {
    pole: f64,
    input: f64,
    output: f64,
}

impl DcBlock {
    pub(crate) fn new(frequency: f64, rate: f64) -> Self {
        Self {
            pole: (-TAU * frequency / rate).exp(),
            input: 0.0,
            output: 0.0,
        }
    }

    /// Moves the corner to `frequency`, keeping the state.
    pub(crate) fn set_frequency(&mut self, frequency: f64, rate: f64) {
        self.pole = (-TAU * frequency / rate).exp();
    }

    #[inline]
    pub(crate) fn step(&mut self, x: f64) -> f64 {
        self.output = flush(x - self.input + self.pole * self.output);
        self.input = x;
        self.output
    }

    /// The gain at `w` radians per sample.
    pub(crate) fn gain(&self, w: f64) -> f64 {
        let numerator = 2.0 * (0.5 * w).sin();
        let denominator = (1.0 - 2.0 * self.pole * w.cos() + self.pole * self.pole).sqrt();
        numerator / denominator.max(1e-12)
    }

    /// The phase delay at `w`, in samples: negative, a high-pass leads.
    pub(crate) fn delay(&self, w: f64) -> f64 {
        let w = w.max(1e-9);
        let phase = 0.5 * core::f64::consts::PI
            - 0.5 * w
            - (self.pole * w.sin()).atan2(1.0 - self.pole * w.cos());
        -phase / w
    }

    pub(crate) fn reset(&mut self) {
        self.input = 0.0;
        self.output = 0.0;
    }
}

/// How a player's vibrato is shaped: its rate, its depth, how long after
/// the start of a note it begins and how long it takes to grow.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct VibratoShape {
    pub(crate) rate: f64,
    pub(crate) depth_cents: f64,
    pub(crate) delay: f64,
    pub(crate) rise: f64,
    /// How much the rate and depth wander, 0 to 1.
    pub(crate) wander: f64,
    /// How much the vibrato develops as it grows, 0 to 1: at 1 it starts
    /// [`DEVELOP_RATE`] slower than its rate and quickens to it as it
    /// reaches its depth, as a player's hand loosens into it. At 0 its rate
    /// is its rate from the start.
    pub(crate) develop: f64,
}

/// How much slower than its rate a vibrato that develops starts.
pub(crate) const DEVELOP_RATE: f64 = 0.2;

/// A vibrato as a player makes it: it waits, grows, and wanders a little
/// in rate and depth, by a repeatable amount seeded by the note.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Vibrato {
    phase: f64,
    step: f64,
    depth: f64,
    wait: f64,
    grow: f64,
    ramp: f64,
    wander: f64,
    develop: f64,
    /// A slowly moving random value and the noise it follows.
    drift: f64,
    drift_target: f64,
    drift_follow: f64,
    drift_countdown: u32,
    drift_period: u32,
    noise: Noise,
}

impl Vibrato {
    pub(crate) fn new() -> Self {
        Self {
            phase: 0.0,
            step: 0.0,
            depth: 0.0,
            wait: 0.0,
            grow: 1.0,
            ramp: 0.0,
            wander: 0.0,
            develop: 0.0,
            drift: 0.0,
            drift_target: 0.0,
            drift_follow: 0.0,
            drift_countdown: 0,
            drift_period: 1,
            noise: Noise::for_note(0, 0.0, 0),
        }
    }

    /// Starts the vibrato of a new note.
    pub(crate) fn start(&mut self, shape: &VibratoShape, rate: f64, seed: Noise) {
        self.noise = seed;
        self.phase = self.noise.unit();
        self.ramp = 0.0;
        self.wait = (shape.delay * rate).max(0.0);
        self.drift = 0.0;
        self.drift_target = 0.0;
        self.drift_countdown = 0;
        self.shape(shape, rate);
    }

    /// Takes a new shape without starting again, as a legato note does.
    pub(crate) fn shape(&mut self, shape: &VibratoShape, rate: f64) {
        self.step = shape.rate / rate;
        self.depth = shape.depth_cents;
        self.grow = 1.0 / (shape.rise * rate).max(1.0);
        self.wander = shape.wander;
        self.develop = shape.develop.clamp(0.0, 1.0);
        // A new random target five times a second, followed smoothly.
        self.drift_period = ((0.2 * rate) as u32).max(1);
        self.drift_follow = 1.0 - (-1.0 / (0.15 * rate)).exp();
    }

    /// The pitch offset for this sample, in cents, with the depth scaled
    /// by `scale`.
    #[inline]
    pub(crate) fn next(&mut self, scale: f64) -> f64 {
        if self.drift_countdown == 0 {
            self.drift_countdown = self.drift_period;
            self.drift_target = self.noise.next();
        }
        self.drift_countdown -= 1;
        self.drift += (self.drift_target - self.drift) * self.drift_follow;
        let wobble = self.wander * self.drift;
        if self.wait > 0.0 {
            self.wait -= 1.0;
        } else if self.ramp < 1.0 {
            self.ramp = (self.ramp + self.grow).min(1.0);
        }
        // An eased ramp, so the vibrato grows in without a corner.
        let ease = self.ramp * self.ramp * (3.0 - 2.0 * self.ramp);
        let developing = 1.0 - DEVELOP_RATE * self.develop * (1.0 - ease);
        self.phase += self.step * developing * (1.0 + 0.08 * wobble);
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
        self.depth * scale * ease * (1.0 + 0.2 * wobble) * (TAU * self.phase).sin()
    }
}

/// A pitch, in keys, that glides to where it is sent.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Glide {
    pub(crate) key: f64,
    pub(crate) target: f64,
    follow: f64,
}

impl Glide {
    pub(crate) fn new() -> Self {
        Self {
            key: 60.0,
            target: 60.0,
            follow: 1.0,
        }
    }

    /// Jumps to `key`.
    pub(crate) fn set(&mut self, key: f64) {
        self.key = key;
        self.target = key;
    }

    /// Glides to `key`, 95% of the way in `seconds`.
    pub(crate) fn to(&mut self, key: f64, seconds: f64, rate: f64) {
        self.target = key;
        self.follow = if seconds > 0.0 {
            1.0 - (-3.0 / (seconds * rate)).exp()
        } else {
            1.0
        };
    }

    #[inline]
    pub(crate) fn next(&mut self) -> f64 {
        let gap = self.target - self.key;
        self.key = if gap.abs() < 1e-9 {
            self.target
        } else {
            self.key + gap * self.follow
        };
        self.key
    }

    /// Whether the glide is still moving.
    pub(crate) fn moving(&self) -> bool {
        self.key != self.target
    }
}

/// A change of a waveguide's length that happens at once, as a key, a valve
/// or a finger changes it: the read at the old length fades into the read at
/// the new one over a few milliseconds, instead of gliding through the
/// lengths between (which is heard as a portamento).
///
/// The wave already in the line is the old note's; read at the new length it
/// no longer fits, and the loop reorganises itself over a few trips round,
/// as the air in a bore or a string does when its length jumps. The fade
/// only keeps the read from stepping, which would be a click.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Switch {
    /// How far the fade has gone, 0 to 1 (1 when there is none), and how far
    /// it goes a sample.
    progress: f64,
    step: f64,
}

impl Switch {
    pub(crate) fn new() -> Self {
        Self {
            progress: 1.0,
            step: 1.0,
        }
    }

    /// Starts a fade of `seconds`.
    pub(crate) fn start(&mut self, seconds: f64, rate: f64) {
        self.progress = 0.0;
        self.step = 1.0 / (seconds * rate).max(1.0);
    }

    /// Ends any fade.
    pub(crate) fn finish(&mut self) {
        self.progress = 1.0;
    }

    /// Whether a fade is under way.
    #[inline]
    pub(crate) fn fading(&self) -> bool {
        self.progress < 1.0
    }

    /// The share of the new read for this sample, eased so that the fade
    /// starts and ends without a corner, and the fade moved on.
    #[inline]
    pub(crate) fn next(&mut self) -> f64 {
        let x = self.progress;
        self.progress = (self.progress + self.step).min(1.0);
        x * x * (3.0 - 2.0 * x)
    }
}

/// A raised-cosine bump: 0, up to 1 half way through `seconds`, and back to
/// 0, once, when started.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Bump {
    left: u32,
    length: u32,
}

impl Bump {
    pub(crate) fn new() -> Self {
        Self { left: 0, length: 1 }
    }

    pub(crate) fn start(&mut self, seconds: f64, rate: f64) {
        self.length = ((seconds * rate) as u32).max(2);
        self.left = self.length;
    }

    pub(crate) fn stop(&mut self) {
        self.left = 0;
    }

    pub(crate) fn running(&self) -> bool {
        self.left > 0
    }

    #[inline]
    pub(crate) fn next(&mut self) -> f64 {
        if self.left == 0 {
            return 0.0;
        }
        self.left -= 1;
        let x = f64::from(self.length - self.left) / f64::from(self.length);
        let s = (core::f64::consts::PI * x).sin();
        s * s
    }
}

/// How long a [`Wander`] holds each random value before it draws the next,
/// as a fraction of its time scale.
const WANDER_HOLD: f64 = 0.25;

/// A slow, smooth random motion with an RMS of 1: what makes a held note
/// move as a player's does, never quite still in pitch or level.
///
/// Random values from the note's own noise, each held a quarter of the time
/// scale, through two one-pole low-passes of that time constant: noise
/// whose spectrum is flat below `1 / (2π τ)` hertz and falls by 12 dB an
/// octave above, so it drifts rather than jitters. Held values of variance
/// `σ²` for `T` seconds have a spectral density of `σ² T` at low
/// frequencies, and the two poles pass `1 / (4τ)` hertz of it, so the
/// output's variance is `σ² T / (4τ)`; the scale makes it 1 (measured
/// within a few percent; see the tests). It starts at 0, so a note starts at
/// its pitch and level and wanders off as it is held.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Wander {
    noise: Noise,
    target: f64,
    first: f64,
    second: f64,
    follow: f64,
    countdown: u32,
    hold: u32,
}

impl Wander {
    pub(crate) fn new() -> Self {
        Self {
            noise: Noise::for_note(0, 0.0, 0),
            target: 0.0,
            first: 0.0,
            second: 0.0,
            follow: 0.0,
            countdown: 0,
            hold: 1,
        }
    }

    /// Starts from 0 with `seed`, on a time scale of `seconds`.
    pub(crate) fn start(&mut self, seed: Noise, seconds: f64, rate: f64) {
        let seconds = seconds.max(1e-3);
        self.noise = seed;
        self.first = 0.0;
        self.second = 0.0;
        self.countdown = 0;
        self.hold = ((WANDER_HOLD * seconds * rate) as u32).max(1);
        self.follow = 1.0 - (-1.0 / (seconds * rate)).exp();
    }

    /// The value for this sample, about -3 to 3 at the most.
    #[inline]
    pub(crate) fn next(&mut self) -> f64 {
        if self.countdown == 0 {
            self.countdown = self.hold;
            // Uniform from -1 to 1 has a variance of a third; held a
            // quarter of the time constant, two poles leave a sixteenth.
            self.target = self.noise.next() * (3.0f64 * 16.0).sqrt();
        }
        self.countdown -= 1;
        self.first = flush(self.first + (self.target - self.first) * self.follow);
        self.second = flush(self.second + (self.first - self.second) * self.follow);
        self.second
    }
}

/// How a held note breathes: it swells after its attack and eases back, as
/// a player leans into a long note and then lets it settle. The difference
/// of two decays, `e^(-t/τ₂) - e^(-t/τ₁)`, normalised to peak at 1: 0 at
/// the start, 1 after 0.87 s, back towards 0 over several seconds.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Swell {
    rise: f64,
    ease: f64,
    rise_factor: f64,
    ease_factor: f64,
}

/// The time constants of a [`Swell`]'s two decays, and where and how high
/// their difference peaks (`t* = τ₁ τ₂ ln(τ₂/τ₁) / (τ₂ - τ₁)`).
const SWELL_RISE: f64 = 0.4;
const SWELL_EASE: f64 = 2.5;
#[cfg(test)]
const SWELL_PEAK: f64 = SWELL_RISE * SWELL_EASE * 1.832_581_463_748_31 / (SWELL_EASE - SWELL_RISE);
const SWELL_HEIGHT: f64 = 0.592_492_820_647_535;

impl Swell {
    pub(crate) fn new() -> Self {
        Self {
            rise: 0.0,
            ease: 0.0,
            rise_factor: 0.0,
            ease_factor: 0.0,
        }
    }

    pub(crate) fn start(&mut self, rate: f64) {
        self.rise = 1.0;
        self.ease = 1.0;
        self.rise_factor = (-1.0 / (SWELL_RISE * rate)).exp();
        self.ease_factor = (-1.0 / (SWELL_EASE * rate)).exp();
    }

    /// The swell for this sample, 0 to 1.
    #[inline]
    pub(crate) fn next(&mut self) -> f64 {
        self.rise = flush(self.rise * self.rise_factor);
        self.ease = flush(self.ease * self.ease_factor);
        (self.ease - self.rise) / SWELL_HEIGHT
    }
}

/// What makes a held note sound played rather than generated: a slow drift
/// of pitch, a slow waver of level and the swell of a long note, each from
/// its own noise, seeded by the note so that a render is the same every
/// time.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Life {
    pitch: Wander,
    level: Wander,
    swell: Swell,
}

/// The time scales of the pitch's drift and the level's waver: under a
/// hertz, slower than any vibrato.
pub(crate) const DRIFT_SECONDS: f64 = 0.35;
pub(crate) const WAVER_SECONDS: f64 = 0.15;

/// One sample of [`Life`]: the drift and the waver, each of RMS 1, and the
/// swell, 0 to 1.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Lively {
    pub(crate) drift: f64,
    pub(crate) waver: f64,
    pub(crate) swell: f64,
}

impl Life {
    pub(crate) fn new() -> Self {
        Self {
            pitch: Wander::new(),
            level: Wander::new(),
            swell: Swell::new(),
        }
    }

    /// Starts a fresh note's life from `seed`.
    pub(crate) fn start(&mut self, mut seed: Noise, rate: f64) {
        let pitch = Noise::for_note(0, seed.next(), 7);
        let level = Noise::for_note(0, seed.next(), 11);
        self.pitch.start(pitch, DRIFT_SECONDS, rate);
        self.level.start(level, WAVER_SECONDS, rate);
        self.swell.start(rate);
    }

    #[inline]
    pub(crate) fn next(&mut self) -> Lively {
        Lively {
            drift: self.pitch.next(),
            waver: self.level.next(),
            swell: self.swell.next(),
        }
    }
}

/// How hard a sustained note is being played, as the player shapes it while
/// it sounds: the breath of a wind instrument, the bow's speed on a string,
/// the effort of a singer.
///
/// A note starts at the level its velocity gives. Pressure on the key
/// ([`Expression::Pressure`]) and the breath controller (controller 2) take
/// over from it once they move, and the expression pedal (controller 11)
/// scales whatever drives the note, as on a MIDI wind controller. The
/// level heard follows smoothly, so no controller can make it step.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Dynamics {
    struck: f64,
    pressure: Option<f64>,
    breath: Option<f64>,
    expression: Option<f64>,
    /// The note's own gain, [`Expression::Volume`].
    pub(crate) volume: f64,
    /// [`Expression::Vibrato`], as a scale of the preset's depth: 1 unless
    /// it moves, then 0 to 2.
    pub(crate) vibrato: f64,
    /// [`Expression::Brightness`], 0 to 1, if it has moved.
    pub(crate) brightness: Option<f64>,
    level: f64,
    follow: f64,
}

impl Dynamics {
    pub(crate) fn new() -> Self {
        Self {
            struck: 0.0,
            pressure: None,
            breath: None,
            expression: None,
            volume: 1.0,
            vibrato: 1.0,
            brightness: None,
            level: 0.0,
            follow: 1.0,
        }
    }

    /// Takes the sample rate, and how long the level takes to follow a
    /// controller (its time constant): a breath answers in a few
    /// milliseconds, a bowing arm in tens.
    pub(crate) fn prepare(&mut self, rate: f64, seconds: f64) {
        self.follow = 1.0 - (-1.0 / (seconds * rate)).exp();
    }

    /// Back to a new note's state, controllers and all.
    pub(crate) fn reset(&mut self) {
        let follow = self.follow;
        *self = Self::new();
        self.follow = follow;
    }

    /// A note starts, or slurs, at `level`: 1 for full velocity, more for
    /// an accent, up to 16.
    pub(crate) fn strike(&mut self, level: f64, fresh: bool) {
        self.struck = setting(level, 0.0, 16.0, 0.0);
        if fresh {
            self.level = self.target();
        }
    }

    fn target(&self) -> f64 {
        let drive = self.pressure.or(self.breath).unwrap_or(self.struck);
        drive * self.expression.unwrap_or(1.0)
    }

    /// The level for this sample, 0 to 1.
    #[inline]
    pub(crate) fn next(&mut self) -> f64 {
        self.level = flush(self.level + (self.target() - self.level) * self.follow);
        self.level
    }

    /// Takes a note expression; returns whether it was one of these.
    pub(crate) fn expression(&mut self, expression: Expression, value: f64) -> bool {
        if !value.is_finite() {
            return false;
        }
        match expression {
            Expression::Pressure => self.pressure = Some(value.clamp(0.0, 1.0)),
            Expression::Volume => self.volume = value.clamp(0.0, 4.0),
            Expression::Vibrato => self.vibrato = 2.0 * value.clamp(0.0, 1.0),
            Expression::Brightness => self.brightness = Some(value.clamp(0.0, 1.0)),
            _ => return false,
        }
        true
    }

    /// Takes the breath controller, the expression pedal and the modulation
    /// wheel (as vibrato depth).
    pub(crate) fn midi(&mut self, message: MidiMessage) {
        if let MidiMessage::ControlChange {
            controller, value, ..
        } = message
        {
            let value = f64::from(value.min(127)) / 127.0;
            match controller {
                1 => self.vibrato = 2.0 * value,
                2 => self.breath = Some(value),
                11 => self.expression = Some(value),
                _ => {}
            }
        }
    }
}

/// Measurements the tests of the physical models share.
#[cfg(test)]
pub(crate) mod probe {
    use crate::instruments::measure::{RATE, amplitude_at, between, frequency_near};

    /// The frequency near `expected` in windows of `window` seconds every
    /// `hop` seconds from `from` to `to`.
    pub(crate) fn pitch_track(
        signal: &[f64],
        expected: f64,
        from: f64,
        to: f64,
        window: f64,
        hop: f64,
    ) -> Vec<f64> {
        let mut track = Vec::new();
        let mut t = from;
        while t + window <= to + 1e-9 {
            track.push(frequency_near(between(signal, t, t + window), expected));
            t += hop;
        }
        track
    }

    /// The pitch period by period: `signal` through a band-pass from
    /// `low` to `high` hertz (around the fundamentals of the notes it
    /// plays, so that only they cross zero), then the time of each upward
    /// crossing of zero, to a fraction of a sample, and the frequency the
    /// time since the last makes. Each entry is `(seconds, hertz)`, at the
    /// crossing that ends the period.
    pub(crate) fn period_track(signal: &[f64], low: f64, high: f64) -> Vec<(f64, f64)> {
        let filtered = crate::instruments::measure::band_passed(signal, low, high);
        let mut track = Vec::new();
        let mut last: Option<f64> = None;
        for (n, pair) in filtered.windows(2).enumerate() {
            if pair[0] < 0.0 && pair[1] >= 0.0 {
                let at = n as f64 + pair[0] / (pair[0] - pair[1]);
                if let Some(before) = last {
                    track.push((at / RATE, RATE / (at - before)));
                }
                last = Some(at);
            }
        }
        track
    }

    /// The RMS level of `signal` above `corner` hertz in successive windows
    /// of `seconds`: where a click or a burst of noise would show.
    pub(crate) fn high_band_levels(signal: &[f64], corner: f64, seconds: f64) -> Vec<f64> {
        let filtered =
            crate::instruments::measure::band_passed(signal, corner, (0.45 * RATE).max(corner));
        levels(&filtered, seconds)
    }

    /// The amplitudes of the first `count` harmonics of `f0`.
    pub(crate) fn harmonics(signal: &[f64], f0: f64, count: usize) -> Vec<f64> {
        (1..=count)
            .map(|n| amplitude_at(signal, f0 * n as f64))
            .collect()
    }

    /// The centroid of the first 40 harmonics of `f0` below 0.45 of the
    /// sample rate, weighted by their amplitudes: the brightness of a
    /// pitched, sustained tone, deaf to the noise and leakage between the
    /// harmonics that a spectrum's centroid adds up.
    pub(crate) fn harmonic_centroid(signal: &[f64], f0: f64) -> f64 {
        let count = ((0.45 * RATE / f0) as usize).clamp(1, 40);
        let amplitudes = harmonics(signal, f0, count);
        let total: f64 = amplitudes.iter().sum();
        let weighted: f64 = amplitudes
            .iter()
            .enumerate()
            .map(|(n, a)| (n + 1) as f64 * f0 * a)
            .sum();
        weighted / total.max(1e-300)
    }

    /// The RMS level of successive windows of `seconds`.
    pub(crate) fn levels(signal: &[f64], seconds: f64) -> Vec<f64> {
        let window = ((seconds * RATE) as usize).max(1);
        signal
            .chunks(window)
            .filter(|chunk| chunk.len() == window)
            .map(crate::instruments::measure::rms)
            .collect()
    }

    /// The rate and the depth (half the swing, in cents) of a pitch track
    /// taken every `hop` seconds around `centre` hertz: the depth from the
    /// spread of the track about a straight line through it (so that a
    /// pitch settling slowly is not taken for vibrato), the rate from how
    /// often it crosses that line.
    pub(crate) fn modulation(track: &[f64], centre: f64, hop: f64) -> (f64, f64) {
        let cents: Vec<f64> = track.iter().map(|f| 1200.0 * (f / centre).log2()).collect();
        let count = cents.len() as f64;
        let mean_t = 0.5 * (count - 1.0);
        let mean = cents.iter().sum::<f64>() / count;
        let slope = cents
            .iter()
            .enumerate()
            .map(|(n, c)| (n as f64 - mean_t) * (c - mean))
            .sum::<f64>()
            / cents
                .iter()
                .enumerate()
                .map(|(n, _)| (n as f64 - mean_t).powi(2))
                .sum::<f64>()
                .max(1e-12);
        let cents: Vec<f64> = cents
            .iter()
            .enumerate()
            .map(|(n, c)| c - mean - slope * (n as f64 - mean_t))
            .collect();
        let rms = (cents.iter().map(|c| c * c).sum::<f64>() / count).sqrt();
        let mean = 0.0;
        let crossings = cents
            .windows(2)
            .filter(|pair| (pair[0] - mean) * (pair[1] - mean) < 0.0)
            .count();
        let seconds = hop * (cents.len() - 1) as f64;
        (
            crossings as f64 / (2.0 * seconds),
            rms * core::f64::consts::SQRT_2,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DelayLine;

    #[test]
    fn the_cubic_read_is_in_tune_where_it_is_asked() {
        // Push a sine through a line and read it back: the phase delay
        // measured is the one predicted, and the corrected delay is the one
        // asked for.
        // Up to 0.15 of the sample rate, the highest note a waveguide here
        // plays.
        for w in [0.01, 0.2, 0.6, 0.94] {
            for target in [1.3, 2.0, 2.5, 7.25, 30.9] {
                let delay = cubic_delay_for(target, w);
                let predicted = cubic_phase_delay(delay, w);
                assert!(
                    (predicted - target).abs() < 1e-6,
                    "{w} {target}: {predicted}"
                );
                let mut line = DelayLine::<f64>::new(64);
                let (mut re, mut im) = (0.0, 0.0);
                let (start, len) = (100, 20_000);
                for n in 0..start + len {
                    line.push((w * n as f64).sin());
                    if n >= start {
                        // The component at w of the read against the input's
                        // phase at the same sample, through a Hann window so
                        // that the part of a period left over does not leak.
                        let y = line.read_cubic(delay);
                        let k = (n - start) as f64 / len as f64;
                        let weight = 0.5 - 0.5 * (TAU * k).cos();
                        let phase = w * n as f64;
                        re += weight * y * phase.sin();
                        im += weight * y * phase.cos();
                    }
                }
                // Read after the push, the newest sample is a delay of 1:
                // the read lags the input by its delay less one.
                let lag = (-im).atan2(re) / w;
                let expected = target - 1.0;
                let turn = TAU / w;
                let error = (lag - expected + 0.5 * turn).rem_euclid(turn) - 0.5 * turn;
                assert!(
                    error.abs() < 1e-3,
                    "{w} {target}: lag {lag} against {expected}"
                );
            }
        }
    }

    #[test]
    fn the_loss_filter_has_the_gains_it_was_designed_for() {
        let (w0, wh) = (0.02, 0.4);
        let loss = Loss::design(0.995, 0.9, w0, wh);
        assert!((loss.gain(w0) - 0.995).abs() < 1e-9);
        assert!((loss.gain(wh) - 0.9).abs() < 1e-9);
        assert!(loss.delay(w0) > 0.0);
        // Flat when asked to be, and finite for anything.
        let flat = Loss::design(0.99, 0.99, w0, wh);
        assert_eq!(flat.a, 0.0);
        for (g0, gh) in [(f64::NAN, 0.5), (0.9, f64::NAN), (2.0, -1.0), (0.0, 0.0)] {
            let loss = Loss::design(g0, gh, w0, wh);
            assert!(loss.b0.is_finite() && loss.a.is_finite());
            assert!(loss.gain(w0) < 1.0 && loss.gain(wh) < 1.0);
        }
        assert!((trip_gain(1.0, 480.0, 48_000.0).powi(100) - 1e-3).abs() < 1e-12);
    }

    #[test]
    fn the_vibrato_waits_grows_and_swings_at_its_rate() {
        let rate = 48_000.0;
        let shape = VibratoShape {
            rate: 5.0,
            depth_cents: 20.0,
            delay: 0.5,
            rise: 0.5,
            wander: 0.0,
            develop: 0.0,
        };
        let mut vibrato = Vibrato::new();
        vibrato.start(&shape, rate, Noise::for_note(60, 0.5, 3));
        let cents: Vec<f64> = (0..(3.0 * rate) as usize)
            .map(|_| vibrato.next(1.0))
            .collect();
        assert!(cents[..(0.5 * rate) as usize].iter().all(|&c| c == 0.0));
        let late = &cents[(1.0 * rate) as usize..];
        let peak = late.iter().fold(0.0f64, |m, c| m.max(c.abs()));
        assert!((peak - 20.0).abs() < 0.05, "{peak}");
        let crossings = late.windows(2).filter(|p| p[0] * p[1] < 0.0).count();
        // Two seconds at 5 Hz: twenty periods.
        assert!((crossings as f64 - 20.0).abs() <= 1.0, "{crossings}");
    }

    #[test]
    fn a_developing_vibrato_starts_slower_and_quickens_to_its_rate() {
        let rate = 48_000.0;
        let shape = VibratoShape {
            rate: 5.0,
            depth_cents: 20.0,
            delay: 0.0,
            rise: 4.0,
            wander: 0.0,
            develop: 1.0,
        };
        let mut vibrato = Vibrato::new();
        vibrato.start(&shape, rate, Noise::for_note(60, 0.5, 3));
        let cents: Vec<f64> = (0..(9.0 * rate) as usize)
            .map(|_| vibrato.next(1.0))
            .collect();
        let rate_over = |from: f64, to: f64| {
            let part = &cents[(from * rate) as usize..(to * rate) as usize];
            let crossings = part.windows(2).filter(|p| p[0] * p[1] < 0.0).count();
            crossings as f64 / (2.0 * (to - from))
        };
        let (early, late) = (rate_over(0.05, 1.65), rate_over(5.0, 9.0));
        println!("developing vibrato: {early:.2} Hz as it grows, {late:.2} Hz grown");
        // Over its first 1.6 s the vibrato's ease rises to about 0.37: it
        // runs at 0.8 to 0.87 of its rate, then at its rate.
        assert!(early < 0.92 * 5.0 && early > 0.75 * 5.0, "{early}");
        assert!((late - 5.0).abs() < 0.2, "{late}");
    }

    #[test]
    fn a_wander_moves_slowly_with_an_rms_of_one() {
        let rate = 48_000.0;
        let mut total = 0.0;
        let mut count = 0.0;
        let mut fast = 0.0f64;
        for seed in 0..8 {
            let mut wander = Wander::new();
            wander.start(Noise::for_note(60, 0.0, seed), DRIFT_SECONDS, rate);
            let values: Vec<f64> = (0..(60.0 * rate) as usize).map(|_| wander.next()).collect();
            // It starts at nothing, then wanders.
            assert!(values[0].abs() < 1e-3);
            total += values.iter().map(|x| x * x).sum::<f64>();
            count += values.len() as f64;
            // Slowly: from one millisecond to the next it moves by a tiny
            // fraction of its swing.
            let step = values
                .windows(48)
                .step_by(48)
                .map(|w| (w[47] - w[0]).abs())
                .fold(0.0f64, f64::max);
            fast = fast.max(step);
        }
        let rms = (total / count).sqrt();
        println!("wander: RMS {rms:.3}, largest change in a millisecond {fast:.4}");
        assert!((rms - 1.0).abs() < 0.15, "{rms}");
        assert!(fast < 0.05, "{fast}");
    }

    #[test]
    fn a_swell_peaks_once_and_eases_back() {
        let rate = 48_000.0;
        let mut swell = Swell::new();
        swell.start(rate);
        let values: Vec<f64> = (0..(8.0 * rate) as usize).map(|_| swell.next()).collect();
        let (at, peak) = values
            .iter()
            .enumerate()
            .fold((0, 0.0f64), |b, (n, &x)| if x > b.1 { (n, x) } else { b });
        assert!(values[0] < 1e-3);
        assert!((peak - 1.0).abs() < 1e-3, "{peak}");
        assert!((at as f64 / rate - SWELL_PEAK).abs() < 0.01, "{at}");
        assert!(values[values.len() - 1] < 0.1);
    }

    #[test]
    fn a_switch_fades_smoothly_and_a_bump_rises_and_falls() {
        let rate = 48_000.0;
        let mut switch = Switch::new();
        assert!(!switch.fading());
        switch.start(0.003, rate);
        let shares: Vec<f64> = (0..200).map(|_| switch.next()).collect();
        assert_eq!(shares[0], 0.0);
        assert!(
            shares
                .windows(2)
                .all(|p| p[1] >= p[0] && p[1] - p[0] < 0.011)
        );
        assert_eq!(shares[199], 1.0);
        assert!(!switch.fading());
        let mut bump = Bump::new();
        bump.start(0.01, rate);
        let values: Vec<f64> = (0..600).map(|_| bump.next()).collect();
        let peak = values.iter().fold(0.0f64, |m, &x| m.max(x));
        assert!((peak - 1.0).abs() < 1e-6);
        assert!(values[0] < 1e-3 && values[479].abs() < 1e-9 && values[599] == 0.0);
        assert!(!bump.running());
    }

    #[test]
    fn dynamics_follow_the_velocity_then_the_controllers() {
        let mut dynamics = Dynamics::new();
        dynamics.prepare(48_000.0, 0.005);
        dynamics.strike(0.5, true);
        assert_eq!(dynamics.next(), 0.5);
        dynamics.expression(Expression::Pressure, 0.25);
        let mut level = 0.0;
        for _ in 0..4_800 {
            level = dynamics.next();
        }
        assert!((level - 0.25).abs() < 1e-6);
        dynamics.midi(MidiMessage::ControlChange {
            channel: 0,
            controller: 11,
            value: 0,
        });
        for _ in 0..4_800 {
            level = dynamics.next();
        }
        assert!(level.abs() < 1e-6);
        assert!(!dynamics.expression(Expression::Pan, 0.5));
        assert!(!dynamics.expression(Expression::Volume, f64::NAN));
        dynamics.reset();
        dynamics.strike(0.7, true);
        assert_eq!(dynamics.next(), 0.7);
    }
}
