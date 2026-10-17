//! Modal synthesis: a struck body as the sum of its modes, each a resonance
//! with its own frequency, decay and share of the strike.
//!
//! The method is the one of J.-M. Adrien, "The missing link: modal
//! synthesis", in G. De Poli, A. Piccialli and C. Roads (eds.),
//! "Representations of Musical Signals", MIT Press, 1991: a linear body
//! struck at one point and heard at another rings in each of its modes
//! with an amplitude given by the mode's shape at the two points, so a
//! bank of resonances, one per mode, is the body.
//!
//! Two banks are here. [`Phasors`] is a few dozen complex one-pole
//! resonators whose frequencies can glide together, for drum heads, whose
//! pitch falls as their swing does. [`Plate`] is hundreds of real two-pole
//! resonators in frequency bands that can be fed noise, for cymbals: the
//! energy a stick puts into the low modes of a cymbal spreads to its high
//! ones over the next tens of milliseconds, and the bank's bands model
//! that.

use core::f64::consts::PI;

use super::common::{Noise, SILENCE, flush, t60_factor};

/// The zeros of the Bessel functions of the first kind below 18, as `(m,
/// n, j)`: `j` is the `n`th zero of `J_m`, in increasing order of `j`.
///
/// An ideal circular membrane of radius `a`, tension `T` and mass per area
/// `σ` fixed at its rim vibrates in modes `(m, n)` with `m` nodal diameters
/// and `n` nodal circles (the rim among them) at `f = j_mn / (2πa) ·
/// sqrt(T / σ)`; its mode shapes are `J_m(j_mn r / a) cos(mθ)`. The
/// frequencies over the lowest one go 1, 1.593, 2.136, 2.295, 2.653, 2.917,
/// 3.155, 3.500 and so on: N. H. Fletcher and T. D. Rossing, "The Physics
/// of Musical Instruments", 2nd ed., Springer 1998, section 3.5, and T. D.
/// Rossing, "Science of Percussion Instruments", World Scientific 2000,
/// chapter 2. The values are those of M. Abramowitz and I. A. Stegun,
/// "Handbook of Mathematical Functions", table 9.5; a test checks every one
/// of them against `J_m` and that none below 18 is missing.
pub(crate) const BESSEL_ZEROS: [(u8, u8, f64); 39] = [
    (0, 1, 2.404_825_558),
    (1, 1, 3.831_705_970),
    (2, 1, 5.135_622_302),
    (0, 2, 5.520_078_110),
    (3, 1, 6.380_161_896),
    (1, 2, 7.015_586_670),
    (4, 1, 7.588_342_435),
    (2, 2, 8.417_244_140),
    (0, 3, 8.653_727_913),
    (5, 1, 8.771_483_816),
    (3, 2, 9.761_023_130),
    (6, 1, 9.936_109_524),
    (1, 3, 10.173_468_135),
    (4, 2, 11.064_709_488),
    (7, 1, 11.086_370_019),
    (2, 3, 11.619_841_172),
    (0, 4, 11.791_534_439),
    (8, 1, 12.225_092_264),
    (5, 2, 12.338_604_197),
    (3, 3, 13.015_200_720),
    (1, 4, 13.323_691_936),
    (9, 1, 13.354_300_477),
    (6, 2, 13.589_290_170),
    (4, 3, 14.372_536_671),
    (10, 1, 14.475_500_686),
    (2, 4, 14.795_951_782),
    (7, 2, 14.821_268_727),
    (0, 5, 14.930_917_708),
    (11, 1, 15.589_847_884),
    (5, 3, 15.700_174_079),
    (8, 2, 16.037_774_190),
    (3, 4, 16.223_466_160),
    (1, 5, 16.470_630_051),
    (12, 1, 16.698_249_933),
    (6, 3, 17.003_819_667),
    (9, 2, 17.241_220_382),
    (4, 4, 17.615_966_050),
    (13, 1, 17.801_435_153),
    (2, 5, 17.959_819_495),
];

/// `J_m(x)`, the Bessel function of the first kind, from its power series.
///
/// For the arguments a membrane needs (`x` up to about 20) the largest
/// term is under 10^8, so the sum keeps nine or more significant digits.
/// It takes a few hundred operations and does not allocate.
pub(crate) fn bessel_j(m: u32, x: f64) -> f64 {
    let half = 0.5 * x;
    // (x/2)^m / m!
    let mut term = 1.0;
    for k in 1..=m {
        term *= half / f64::from(k);
    }
    let step = -half * half;
    let mut sum = term;
    for k in 1..120u32 {
        term *= step / (f64::from(k) * f64::from(k + m));
        sum += term;
        if f64::from(k) > half && term.abs() <= 1e-17 * sum.abs() {
            break;
        }
    }
    sum
}

/// The force of a stick, a beater or a hand on the body it strikes: half a
/// sine raised to the power 3/2 over the time of contact, as the contact
/// of two elastic bodies has it (Hertz; Fletcher and Rossing, section 18.1).
/// Its samples add up to the strike's level, so a mode much lower than the
/// inverse of the contact time takes the whole of it, and modes above
/// lose more the longer the contact is: a soft or slow strike is dark.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Pulse {
    left: u32,
    len: u32,
    /// The level over the sum of the shape's samples.
    scale: f64,
}

impl Pulse {
    /// Starts a pulse of `level` lasting `seconds`, at least one sample.
    pub(crate) fn start(&mut self, seconds: f64, level: f64, rate: f64) {
        let len = (seconds * rate).round().clamp(1.0, 4_096.0) as u32;
        let mut sum = 0.0;
        for k in 0..len {
            sum += Self::shape(k, len);
        }
        self.len = len;
        self.left = len;
        self.scale = level / sum;
    }

    #[inline]
    fn shape(k: u32, len: u32) -> f64 {
        let s = (PI * (f64::from(k) + 0.5) / f64::from(len)).sin();
        s * s.sqrt()
    }

    /// Whether any of the pulse is still to come.
    pub(crate) fn is_running(&self) -> bool {
        self.left > 0
    }

    /// The next sample of force, zero once the contact is over.
    #[inline]
    pub(crate) fn next(&mut self) -> f64 {
        if self.left == 0 {
            return 0.0;
        }
        let k = self.len - self.left;
        self.left -= 1;
        self.scale * Self::shape(k, self.len)
    }

    pub(crate) fn stop(&mut self) {
        self.left = 0;
    }
}

/// The most modes a [`Phasors`] bank holds.
pub(crate) const MAX_PHASORS: usize = 96;

/// How many samples apart a gliding bank recomputes its poles.
const GLIDE_HOP: u32 = 16;

/// A glide smaller than this, as a fraction of the frequency, is over.
const GLIDE_END: f64 = 1e-7;

/// A bank of complex one-pole resonators, `z ← p·z + g·x`, each ringing
/// as `Im(z)`: a mode struck by an impulse of `g` rings as `g·r^n·sin(nω)`,
/// so a mode's input gain is its amplitude.
///
/// Every frequency can be raised by the same fraction, which falls away
/// exponentially: a drum head swinging wide is stretched tighter, so it
/// starts sharp and settles (tension modulation; Fletcher and Rossing,
/// section 18.2, and Rossing, chapter 4). The poles are recomputed every
/// [`GLIDE_HOP`] samples of the bank's own count, never at block
/// boundaries, so the output does not depend on the block size. Unlike a
/// two-pole resonator's, the state of a complex one keeps its amplitude
/// when its frequency changes.
///
/// Each mode feeds two outputs with weights of its own: what is heard,
/// and another motion the caller needs (the snare head's, for the wires).
#[derive(Debug, Clone)]
pub(crate) struct Phasors {
    len: usize,
    omega: [f64; MAX_PHASORS],
    radius: [f64; MAX_PHASORS],
    pr: [f64; MAX_PHASORS],
    pi: [f64; MAX_PHASORS],
    zr: [f64; MAX_PHASORS],
    zi: [f64; MAX_PHASORS],
    input: [f64; MAX_PHASORS],
    sound: [f64; MAX_PHASORS],
    other: [f64; MAX_PHASORS],
    /// How far above their frequencies the modes are, as a fraction, and
    /// what that is multiplied by every hop.
    glide: f64,
    glide_fall: f64,
    hop_left: u32,
}

impl Default for Phasors {
    fn default() -> Self {
        Self {
            len: 0,
            omega: [0.0; MAX_PHASORS],
            radius: [0.0; MAX_PHASORS],
            pr: [0.0; MAX_PHASORS],
            pi: [0.0; MAX_PHASORS],
            zr: [0.0; MAX_PHASORS],
            zi: [0.0; MAX_PHASORS],
            input: [0.0; MAX_PHASORS],
            sound: [0.0; MAX_PHASORS],
            other: [0.0; MAX_PHASORS],
            glide: 0.0,
            glide_fall: 0.0,
            hop_left: 0,
        }
    }
}

impl Phasors {
    /// Removes every mode.
    pub(crate) fn clear(&mut self) {
        self.len = 0;
        self.glide = 0.0;
    }

    /// How many modes the bank has.
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// Adds a mode at `hertz` that falls 60 dB in `t60` seconds, heard with
    /// weight `sound` and given to the other output with weight `other`.
    /// A mode at or above 0.45 of the sample rate, or past the bank's
    /// room, is left out. Its state starts at rest and its input gain at
    /// zero.
    pub(crate) fn add(&mut self, hertz: f64, t60: f64, sound: f64, other: f64, rate: f64) {
        if self.len == MAX_PHASORS || !(hertz > 0.0 && hertz < 0.45 * rate) {
            return;
        }
        let i = self.len;
        self.omega[i] = core::f64::consts::TAU * hertz / rate;
        self.radius[i] = t60_factor(t60, rate);
        self.zr[i] = 0.0;
        self.zi[i] = 0.0;
        self.input[i] = 0.0;
        self.sound[i] = sound;
        self.other[i] = other;
        self.len += 1;
        self.set_pole(i);
    }

    fn set_pole(&mut self, i: usize) {
        let (sin, cos) = (self.omega[i] * (1.0 + self.glide)).sin_cos();
        self.pr[i] = self.radius[i] * cos;
        self.pi[i] = self.radius[i] * sin;
    }

    /// The input gains of the modes, in the order they were added: the
    /// share of the next strike each takes.
    pub(crate) fn inputs_mut(&mut self) -> &mut [f64] {
        &mut self.input[..self.len]
    }

    /// The frequency of a mode, in radians a sample, without the glide.
    #[cfg(test)]
    pub(crate) fn omega(&self, index: usize) -> f64 {
        self.omega[index]
    }

    /// The decay time of a mode at `rate`, in seconds.
    #[cfg(test)]
    pub(crate) fn t60(&self, index: usize, rate: f64) -> f64 {
        -core::f64::consts::LN_10 * 3.0 / (rate * self.radius[index].ln())
    }

    /// Raises every frequency by `depth` (a fraction, added to what is left
    /// of an earlier glide) and lets that fall to a thousandth in `t60`
    /// seconds.
    pub(crate) fn glide(&mut self, depth: f64, t60: f64, rate: f64) {
        let depth = depth.clamp(0.0, 1.0);
        if depth <= GLIDE_END && self.glide == 0.0 {
            return;
        }
        self.glide = (self.glide + depth).min(1.0);
        self.glide_fall = t60_factor(t60, rate).powi(GLIDE_HOP as i32);
        self.hop_left = 0;
    }

    /// One sample, with `force` struck in: what is heard and the other
    /// output.
    #[inline]
    pub(crate) fn next(&mut self, force: f64) -> (f64, f64) {
        if self.glide != 0.0 {
            if self.hop_left == 0 {
                self.hop_left = GLIDE_HOP;
                self.glide *= self.glide_fall;
                if self.glide < GLIDE_END {
                    self.glide = 0.0;
                }
                for i in 0..self.len {
                    self.set_pole(i);
                }
            }
            self.hop_left -= 1;
        }
        let len = self.len;
        let (pr, pi) = (&self.pr[..len], &self.pi[..len]);
        let (zr, zi) = (&mut self.zr[..len], &mut self.zi[..len]);
        let (input, sound, other) = (&self.input[..len], &self.sound[..len], &self.other[..len]);
        let (mut heard, mut motion) = (0.0, 0.0);
        for i in 0..len {
            let re = pr[i] * zr[i] - pi[i] * zi[i] + input[i] * force;
            let im = pr[i] * zi[i] + pi[i] * zr[i];
            zr[i] = re;
            zi[i] = im;
            heard += sound[i] * im;
            motion += other[i] * im;
        }
        (heard, motion)
    }

    /// The most the heard output can still reach: the sum of every mode's
    /// amplitude times its weight.
    pub(crate) fn level(&self) -> f64 {
        (0..self.len)
            .map(|i| self.sound[i].abs() * self.zr[i].hypot(self.zi[i]))
            .sum()
    }

    /// Puts every mode to rest, and flushes modes too quiet to matter.
    pub(crate) fn flush_quiet(&mut self) {
        for i in 0..self.len {
            self.zr[i] = flush(self.zr[i]);
            self.zi[i] = flush(self.zi[i]);
        }
    }

    /// Every mode at rest and no glide, the modes kept.
    pub(crate) fn rest(&mut self) {
        self.zr = [0.0; MAX_PHASORS];
        self.zi = [0.0; MAX_PHASORS];
        self.glide = 0.0;
        self.hop_left = 0;
        for i in 0..self.len {
            self.set_pole(i);
        }
    }

    /// Multiplies every frequency by `factor`, each mode keeping its
    /// state, as a bend does: the modes ring on at their new pitch. A mode
    /// sent to 0.45 of the sample rate or above stops there, silent.
    pub(crate) fn scale_frequencies(&mut self, factor: f64) {
        if !(factor > 0.0 && factor.is_finite()) {
            return;
        }
        for i in 0..self.len {
            self.omega[i] *= factor;
            if self.omega[i] >= 0.9 * PI {
                self.omega[i] = 0.9 * PI;
                self.radius[i] = 0.0;
                self.zr[i] = 0.0;
                self.zi[i] = 0.0;
            }
            self.set_pole(i);
        }
    }

    /// Shortens every decay to at most `t60` seconds from now on, as a
    /// damper does.
    pub(crate) fn limit_decay(&mut self, t60: f64, rate: f64) {
        let radius = t60_factor(t60, rate);
        for i in 0..self.len {
            self.radius[i] = self.radius[i].min(radius);
            self.set_pole(i);
        }
    }
}

/// How many frequency bands a [`Plate`] has.
pub(crate) const PLATE_BANDS: usize = 4;

/// The most modes a [`Plate`] holds.
pub(crate) const MAX_PLATE_MODES: usize = 640;

/// How many samples apart a plate measures its bands to see which are
/// still sounding.
const PLATE_CHECK: u32 = 64;

/// A reservoir below this feeds its band nothing.
const FEED_FLOOR: f64 = 1e-12;

/// What sets the modes of a [`Plate`]: how many, where and how long they
/// ring.
///
/// The modes are spread with the density of a thin plate, constant in
/// frequency above `knee_hz` (Fletcher and Rossing, section 3.13: `n(f) =
/// S·sqrt(3) / (h·c_L)`, a mode every 15 to 30 hertz for cymbals), and
/// thinning out to nothing at `low_hz` below it, where the curvature of a
/// cymbal stiffens it. They come in close pairs, as the two orientations of
/// a mode of a circular plate do once the plate is not perfectly round, so
/// they beat. Where each one falls is drawn from `seed`, once: the modes
/// are the cymbal's own, the same every time it is struck.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PlateSpec {
    pub(crate) seed: u64,
    pub(crate) modes: usize,
    pub(crate) low_hz: f64,
    pub(crate) knee_hz: f64,
    pub(crate) high_hz: f64,
    /// How far apart the two modes of a pair are, at most, as a fraction
    /// of their frequency.
    pub(crate) split: f64,
    /// The decay time of a mode by its frequency, as `(hertz, seconds)` in
    /// increasing order of frequency: straight lines between the points on
    /// a logarithmic scale of frequency, flat beyond the ends. Real cymbals
    /// do not decay faster the higher they go: their lowest modes die
    /// first, and the middle of their range rings longest.
    pub(crate) t60: &'static [(f64, f64)],
    /// Where the bands meet, in hertz.
    pub(crate) edges: [f64; PLATE_BANDS - 1],
}

impl PlateSpec {
    /// The decay time of a mode at `hertz`, before any scaling.
    pub(crate) fn t60_at(&self, hertz: f64) -> f64 {
        let points = self.t60;
        let Some(&(first_hz, first)) = points.first() else {
            return 1.0;
        };
        if hertz <= first_hz {
            return first;
        }
        for pair in points.windows(2) {
            let ((low_hz, low), (high_hz, high)) = (pair[0], pair[1]);
            if hertz <= high_hz {
                let x = (hertz / low_hz).ln() / (high_hz / low_hz).ln().max(1e-12);
                return low + (high - low) * x;
            }
        }
        points[points.len() - 1].1
    }
}

/// How a [`Plate`] is struck: the level and spectrum of the direct strike,
/// and the cascade that carries its energy up the bands.
///
/// The strike's share of each mode is `(f / 1 kHz)^tilt`, below `low_cut`
/// falling 12 dB an octave, times a random factor drawn for the strike
/// from a Rayleigh distribution with a random sign (where the stick lands
/// among the mode shapes); the strike's force pulse filters that further.
///
/// The cascade is a chain of reservoirs, one per band: the first is filled
/// by the strike and empties in `hold[0]` (a 60 dB fall); each next one is
/// filled from the one below at the rate `fill[b]` (per second, of the
/// reservoir below) and empties in `hold[b]`; each feeds noise to its
/// band's modes with a level of `gain[b]` times its square root. A
/// reservoir filled from one that is emptying rises and then falls, later
/// the higher it is: the slow growth of high frequencies after a cymbal is
/// struck, which T. D. Rossing describes in "Science of Percussion
/// Instruments", chapter 9, and which A. Chaigne, C. Touzé and O. Thomas
/// trace to the nonlinear coupling of its modes ("Nonlinear vibrations and
/// chaos in gongs and cymbals", Acoustical Science and Technology 26,
/// 2005). The cascade does not take energy from the bands below: it is a
/// model of what is heard, not a conservative one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PlateStrike {
    pub(crate) level: f64,
    pub(crate) tilt: f64,
    pub(crate) low_cut: f64,
    /// Multiplies the strike's share of each band: where on the cymbal it
    /// is struck shapes it more coarsely than the tilt.
    pub(crate) bands: [f64; PLATE_BANDS],
    pub(crate) hold: [f64; PLATE_BANDS],
    pub(crate) fill: [f64; PLATE_BANDS],
    pub(crate) gain: [f64; PLATE_BANDS],
}

/// One band of a [`Plate`]: its modes, and the reservoir feeding them.
#[derive(Debug, Clone, Copy)]
struct PlateBand {
    start: usize,
    end: usize,
    reservoir: f64,
    fall: f64,
    fill: f64,
    gain: f64,
    noise: Noise,
    /// Whether the band's modes still need computing.
    alive: bool,
}

impl Default for PlateBand {
    fn default() -> Self {
        Self {
            start: 0,
            end: 0,
            reservoir: 0.0,
            fall: 0.0,
            fill: 0.0,
            gain: 0.0,
            noise: Noise::for_note(0, 0.0, 0),
            alive: false,
        }
    }
}

/// A band of a [`Plate`]'s modes, to ring for one sample.
struct Modes<'a> {
    a1: &'a [f64],
    a2: &'a [f64],
    y1: &'a mut [f64],
    y2: &'a mut [f64],
}

impl Modes<'_> {
    /// One sample of every mode, with each input's sample times its gains
    /// fed in, and their sum.
    ///
    /// Four modes at a time, with four partial sums, so that the compiler
    /// can keep them in vector registers; the sum of a band is added up in
    /// a fixed order, the same whatever the block size. `N` is a constant,
    /// so a band fed nothing runs the loop with nothing in it.
    #[inline(always)]
    fn ring<const N: usize>(self, inputs: [(&[f64], f64); N]) -> f64 {
        const LANES: usize = 4;
        let len = self.a1.len();
        let (a1, a2) = (self.a1, &self.a2[..len]);
        let (y1, y2) = (&mut self.y1[..len], &mut self.y2[..len]);
        let gains = inputs.map(|(gains, _)| &gains[..len]);
        let xs = inputs.map(|(_, x)| x);
        let mut sums = [0.0; LANES];
        let whole = len - len % LANES;
        for chunk in (0..whole).step_by(LANES) {
            for (lane, sum) in sums.iter_mut().enumerate() {
                let k = chunk + lane;
                let mut y = a1[k] * y1[k] - a2[k] * y2[k];
                for (g, x) in gains.iter().zip(xs) {
                    y += x * g[k];
                }
                y2[k] = y1[k];
                y1[k] = y;
                *sum += y;
            }
        }
        for k in whole..len {
            let mut y = a1[k] * y1[k] - a2[k] * y2[k];
            for (g, x) in gains.iter().zip(xs) {
                y += x * g[k];
            }
            y2[k] = y1[k];
            y1[k] = y;
            sums[0] += y;
        }
        (sums[0] + sums[1]) + (sums[2] + sums[3])
    }
}

/// Hundreds of two-pole resonators, `y = a1·y[-1] − a2·y[-2] + b·x`, in
/// [`PLATE_BANDS`] bands of frequency, for a cymbal or a hi-hat.
///
/// Each mode takes the strike's force pulse with a gain of its own and its
/// band's noise with another. The noise gains are set so that every mode,
/// fed steadily, would ring at the same level whatever its decay, so the
/// cascade's levels are the bands' levels. The modes are sorted by
/// frequency and their decays fall with it, so the high bands fall silent
/// first; a band that is silent and fed nothing is skipped.
#[derive(Debug, Clone)]
pub(crate) struct Plate {
    len: usize,
    /// What the modes were last set from: the spec, tuning, decay and
    /// sample rate.
    spec: Option<(PlateSpec, f64, f64, f64)>,
    hertz: Vec<f64>,
    a1: Vec<f64>,
    a2: Vec<f64>,
    y1: Vec<f64>,
    y2: Vec<f64>,
    strike: Vec<f64>,
    feed: Vec<f64>,
    /// `1 / sin²ω`, to read a mode's amplitude from its state.
    amplitude: Vec<f64>,
    bands: [PlateBand; PLATE_BANDS],
    check_left: u32,
}

impl Plate {
    /// An empty plate with room for [`MAX_PLATE_MODES`]. Allocates.
    pub(crate) fn new() -> Self {
        let room = || vec![0.0; MAX_PLATE_MODES];
        Self {
            len: 0,
            spec: None,
            hertz: room(),
            a1: room(),
            a2: room(),
            y1: room(),
            y2: room(),
            strike: room(),
            feed: room(),
            amplitude: room(),
            bands: [PlateBand::default(); PLATE_BANDS],
            check_left: 0,
        }
    }

    /// Sets the modes from `spec`, tuned by the ratio `tune` and with every
    /// decay time multiplied by `decay`, at `rate`. Does nothing if they
    /// are already set so. Keeps the modes' states, so a plate struck again
    /// with other settings carries on ringing. Does not allocate.
    pub(crate) fn configure(&mut self, spec: &PlateSpec, tune: f64, decay: f64, rate: f64) {
        let key = (*spec, tune, decay, rate);
        if self.spec == Some(key) {
            return;
        }
        let first = self.spec.is_none();
        self.spec = Some(key);
        let count = spec.modes.min(MAX_PLATE_MODES) / 2;
        let mut random = Noise::for_note(0, 0.0, spec.seed);
        // The number of modes below f: rising as the square of f − low up
        // to the knee (a density growing from nothing), then straight.
        let ramp = 0.5 * (spec.knee_hz - spec.low_hz).max(1.0);
        let total = ramp + (spec.high_hz - spec.knee_hz).max(1.0);
        let below = |fraction: f64| {
            let modes = fraction * total;
            if modes < ramp {
                spec.low_hz + (2.0 * ramp * modes).sqrt()
            } else {
                spec.knee_hz + (modes - ramp)
            }
        };
        let mut len = 0;
        for k in 0..count {
            let centre = below((k as f64 + random.unit()) / count as f64) * tune;
            let half = 0.5 * spec.split * random.unit();
            for hertz in [centre * (1.0 - half), centre * (1.0 + half)] {
                if hertz < 0.45 * rate && len < MAX_PLATE_MODES {
                    self.hertz[len] = hertz;
                    len += 1;
                }
            }
        }
        self.hertz[..len].sort_by(f64::total_cmp);
        self.len = len;
        for i in 0..len {
            let hertz = self.hertz[i];
            // Tuned, every mode keeps its decay: the curve moves with the
            // modes.
            let t60 = (spec.t60_at(hertz / tune) * decay).clamp(0.001, 60.0);
            let r = t60_factor(t60, rate);
            let omega = core::f64::consts::TAU * hertz / rate;
            let (sin, cos) = omega.sin_cos();
            let (a1, a2) = (2.0 * r * cos, r * r);
            self.a1[i] = a1;
            self.a2[i] = a2;
            self.amplitude[i] = 1.0 / (sin * sin);
            // The steady output variance of 1 / (1 − a1·z⁻¹ + a2·z⁻²) for
            // white noise of unit variance, and a random weight for the
            // mode, drawn once like its frequency.
            let variance = (1.0 + a2) / ((1.0 - a2) * ((1.0 + a2).powi(2) - a1 * a1));
            self.feed[i] = (0.25 + random.unit()) / variance.max(1e-300).sqrt();
            if first {
                self.y1[i] = 0.0;
                self.y2[i] = 0.0;
            }
        }
        let mut start = 0;
        for (band, slot) in self.bands.iter_mut().enumerate() {
            let edge = spec.edges.get(band).map_or(f64::INFINITY, |&e| e * tune);
            let end = start + self.hertz[start..len].partition_point(|&f| f < edge);
            slot.start = start;
            slot.end = end;
            start = end;
            // The band's noise gains share out its level evenly in power.
            let mut power = 0.0;
            for i in slot.start..slot.end {
                let variance = (1.0 + self.a2[i])
                    / ((1.0 - self.a2[i]) * ((1.0 + self.a2[i]).powi(2) - self.a1[i].powi(2)));
                power += self.feed[i].powi(2) * variance;
            }
            let norm = 1.0 / power.max(1e-300).sqrt();
            for value in &mut self.feed[slot.start..slot.end] {
                *value *= norm;
            }
        }
    }

    /// The frequency of each mode, in increasing order.
    #[cfg(test)]
    pub(crate) fn frequencies(&self) -> &[f64] {
        &self.hertz[..self.len]
    }

    /// Where each band starts and ends, as mode indices.
    #[cfg(test)]
    pub(crate) fn band_range(&self, band: usize) -> (usize, usize) {
        (self.bands[band].start, self.bands[band].end)
    }

    /// Strikes the plate: sets each mode's share of the force pulse to come
    /// and fills the first reservoir, adding to whatever is ringing.
    /// `random` decides the modes' shares. Does not allocate.
    pub(crate) fn strike(&mut self, how: &PlateStrike, random: &mut Noise, rate: f64) {
        let mut power = 0.0;
        for band in 0..PLATE_BANDS {
            let (start, end) = (self.bands[band].start, self.bands[band].end);
            for i in start..end {
                let hertz = self.hertz[i];
                let mut share = (hertz / 1_000.0).powf(how.tilt) * how.bands[band];
                if hertz < how.low_cut {
                    share *= (hertz / how.low_cut).powi(2);
                }
                // Rayleigh-distributed, with a random sign.
                let size = (-2.0 * (1.0 - random.unit()).max(1e-12).ln()).sqrt();
                let sign = if random.next() < 0.0 { -1.0 } else { 1.0 };
                let amplitude = share * size * sign;
                power += 0.5 * amplitude * amplitude;
                // An impulse of b rings at b / sin ω.
                self.strike[i] = amplitude * (self.amplitude[i].sqrt()).recip();
            }
        }
        // The strike rings at `level` RMS before its pulse filters it.
        let norm = how.level / power.max(1e-300).sqrt();
        for value in &mut self.strike[..self.len] {
            *value *= norm;
        }
        for (band, slot) in self.bands.iter_mut().enumerate() {
            slot.fall = t60_factor(how.hold[band], rate);
            slot.fill = how.fill[band] / rate;
            slot.gain = how.gain[band] * how.level;
            slot.alive = slot.end > slot.start;
            if band == 0 {
                slot.reservoir = 1.0;
            }
            slot.noise = Noise::for_note(band as u8, how.level, random.unit().to_bits());
        }
        self.check_left = PLATE_CHECK;
    }

    /// One sample, with `force` struck in.
    #[inline]
    pub(crate) fn next(&mut self, force: f64) -> f64 {
        let mut out = 0.0;
        let mut below = 0.0;
        for band in &mut self.bands {
            band.reservoir = flush(band.reservoir * band.fall + band.fill * below);
            below = band.reservoir;
            let inject = if band.reservoir > FEED_FLOOR {
                band.alive |= band.end > band.start;
                band.gain * band.reservoir.sqrt() * band.noise.next()
            } else {
                0.0
            };
            if !band.alive {
                continue;
            }
            let (start, end) = (band.start, band.end);
            let modes = Modes {
                a1: &self.a1[start..end],
                a2: &self.a2[start..end],
                y1: &mut self.y1[start..end],
                y2: &mut self.y2[start..end],
            };
            // Most of the time nothing is fed in: the cheapest loop then.
            let (strike, feed) = (&self.strike[start..end], &self.feed[start..end]);
            out += if force != 0.0 {
                modes.ring([(strike, force), (feed, inject)])
            } else if inject != 0.0 {
                modes.ring([(feed, inject)])
            } else {
                modes.ring([])
            };
        }
        self.check_left = self.check_left.saturating_sub(1);
        if self.check_left == 0 {
            self.check_left = PLATE_CHECK;
            self.check();
        }
        out
    }

    /// Marks bands silent once their modes are below the threshold of
    /// silence and neither their reservoir nor any below it can feed them,
    /// and puts their modes to rest.
    fn check(&mut self) {
        let mut feeding = false;
        for band in &mut self.bands {
            feeding |= band.reservoir > FEED_FLOOR;
            if !band.alive || feeding {
                continue;
            }
            let (start, end) = (band.start, band.end);
            let mut energy = 0.0;
            for i in start..end {
                let (y1, y2) = (self.y1[i], self.y2[i]);
                energy +=
                    (y1 * y1 - self.a1[i] * y1 * y2 + self.a2[i] * y2 * y2) * self.amplitude[i];
            }
            // The sum of the amplitudes is at most sqrt(count · energy).
            let bound = ((end - start) as f64 * energy.max(0.0)).sqrt();
            if bound < 0.1 * SILENCE {
                band.alive = false;
                band.reservoir = 0.0;
                self.y1[start..end].fill(0.0);
                self.y2[start..end].fill(0.0);
            }
        }
    }

    /// Whether any band is still sounding or about to.
    pub(crate) fn is_sounding(&self) -> bool {
        self.bands.iter().any(|band| band.alive)
    }

    /// Every mode at rest and every reservoir empty; the modes are kept.
    pub(crate) fn rest(&mut self) {
        self.y1[..self.len].fill(0.0);
        self.y2[..self.len].fill(0.0);
        for band in &mut self.bands {
            band.reservoir = 0.0;
            band.alive = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instruments::measure::{RATE, amplitude_at, between, frequency_near, t60_at};

    /// `J_m(x)` from Bessel's integral, `(1/π) ∫₀^π cos(mτ − x sin τ) dτ`,
    /// by the trapezoidal rule, which converges geometrically for a
    /// periodic integrand: an independent check of the series.
    fn bessel_integral(m: u32, x: f64) -> f64 {
        let steps = 256;
        let mut sum = 0.0;
        for k in 0..=steps {
            let tau = PI * f64::from(k) / f64::from(steps);
            let weight = if k == 0 || k == steps { 0.5 } else { 1.0 };
            sum += weight * (f64::from(m) * tau - x * tau.sin()).cos();
        }
        sum / f64::from(steps)
    }

    #[test]
    fn the_bessel_function_matches_its_integral() {
        for m in 0..16 {
            for step in 0..200 {
                let x = 0.1 * f64::from(step);
                let series = bessel_j(m, x);
                let integral = bessel_integral(m, x);
                assert!((series - integral).abs() < 1e-9, "J_{m}({x})");
            }
        }
        assert_eq!(bessel_j(0, 0.0), 1.0);
        assert_eq!(bessel_j(3, 0.0), 0.0);
    }

    #[test]
    fn every_zero_below_18_is_in_the_table_and_is_a_zero() {
        // Every tabulated value is a zero to its nine decimals.
        for &(m, _, j) in &BESSEL_ZEROS {
            let m = u32::from(m);
            let (below, above) = (bessel_integral(m, j - 2e-8), bessel_integral(m, j + 2e-8));
            assert!(below * above <= 0.0, "J_{m} has no zero at {j}");
        }
        // And none is missing: the zeros found by scanning each J_m are the
        // table's, numbered in order.
        let mut found = Vec::new();
        for m in 0..20u32 {
            // J_m is tiny and has no zero below x = m, where rounding would
            // make zeros of nothing.
            let mut n = 0;
            let mut x = f64::from(m).max(0.5);
            let mut last = bessel_integral(m, x);
            while x < 18.0 {
                let next = bessel_integral(m, x + 0.01);
                if last * next < 0.0 {
                    n += 1;
                    found.push((m, n, x));
                }
                last = next;
                x += 0.01;
            }
        }
        assert_eq!(found.len(), BESSEL_ZEROS.len());
        for (m, n, x) in found {
            let entry = BESSEL_ZEROS
                .iter()
                .find(|&&(tm, tn, _)| u32::from(tm) == m && u32::from(tn) == n)
                .unwrap_or_else(|| panic!("({m}, {n}) near {x} is missing"));
            assert!((entry.2 - x).abs() < 0.011, "({m}, {n})");
        }
        // In increasing order, and the ratios Rossing gives.
        assert!(BESSEL_ZEROS.windows(2).all(|pair| pair[0].2 < pair[1].2));
        let ratios = [1.0, 1.593, 2.136, 2.295, 2.653, 2.917, 3.155, 3.500];
        for (zero, ratio) in BESSEL_ZEROS.iter().zip(ratios) {
            assert!((zero.2 / BESSEL_ZEROS[0].2 - ratio).abs() < 0.001);
        }
    }

    #[test]
    fn a_pulse_adds_up_to_its_level_over_its_length() {
        let mut pulse = Pulse::default();
        pulse.start(0.001, 0.7, RATE);
        let samples: Vec<f64> =
            core::iter::from_fn(|| pulse.is_running().then(|| pulse.next())).collect();
        assert_eq!(samples.len(), 48);
        assert!((samples.iter().sum::<f64>() - 0.7).abs() < 1e-12);
        assert!(samples[0] > 0.0 && samples[0] < 0.01 * samples[24]);
        assert_eq!(pulse.next(), 0.0);
    }

    fn ring(bank: &mut Phasors, seconds: f64) -> Vec<f64> {
        (0..(seconds * RATE) as usize)
            .map(|n| bank.next(if n == 0 { 1.0 } else { 0.0 }).0)
            .collect()
    }

    #[test]
    fn phasor_modes_ring_at_their_frequency_and_decay_and_glide_back() {
        let mut bank = Phasors::default();
        bank.add(220.0, 1.5, 1.0, 0.5, RATE);
        bank.add(731.0, 0.6, 0.5, 0.0, RATE);
        bank.add(30_000.0, 1.0, 1.0, 0.0, RATE); // above 0.45 of the rate
        assert_eq!(bank.len(), 2);
        bank.inputs_mut().fill(1.0);
        let signal = ring(&mut bank, 2.0);
        for (hertz, t60) in [(220.0, 1.5), (731.0, 0.6)] {
            let measured = frequency_near(between(&signal, 0.1, 1.0), hertz);
            assert!((measured / hertz - 1.0).abs() < 1e-6, "{measured}");
            let decay = t60_at(&signal, hertz, 0.1, 0.9);
            assert!((decay / t60 - 1.0).abs() < 0.01, "{decay}");
        }
        // The amplitude is the input gain times the weight: over the first
        // 50 ms, what is left at their middle.
        let start = amplitude_at(between(&signal, 0.0, 0.05), 220.0);
        let expected = t60_factor(1.5, RATE).powf(0.025 * RATE);
        assert!((start / expected - 1.0).abs() < 0.01, "{start}");
        // A glide starts sharp and comes back to the same frequency.
        let mut bank = Phasors::default();
        bank.add(220.0, 3.0, 1.0, 0.0, RATE);
        bank.inputs_mut().fill(1.0);
        bank.glide(0.05, 0.2, RATE);
        let signal = ring(&mut bank, 2.0);
        let early = frequency_near(between(&signal, 0.0, 0.03), 230.0);
        assert!(early > 225.0, "{early}");
        let late = frequency_near(between(&signal, 0.5, 1.5), 220.0);
        assert!((late / 220.0 - 1.0).abs() < 1e-6, "{late}");
    }

    fn crash() -> PlateSpec {
        PlateSpec {
            seed: 3,
            modes: 400,
            low_hz: 150.0,
            knee_hz: 800.0,
            high_hz: 18_000.0,
            split: 0.004,
            t60: &[
                (300.0, 0.4),
                (1_000.0, 3.0),
                (3_000.0, 3.6),
                (12_000.0, 2.0),
            ],
            edges: [1_500.0, 4_000.0, 8_000.0],
        }
    }

    #[test]
    fn a_plate_has_the_density_and_decays_it_is_given() {
        // The decay curve: flat beyond its ends, straight between its
        // points on a logarithmic scale of frequency.
        let spec = crash();
        assert_eq!(spec.t60_at(100.0), 0.4);
        assert_eq!(spec.t60_at(20_000.0), 2.0);
        assert!((spec.t60_at(6_000.0) - 2.8).abs() < 1e-9);
        let mut plate = Plate::new();
        plate.configure(&crash(), 1.0, 1.0, RATE);
        let modes = plate.frequencies();
        assert_eq!(modes.len(), 400);
        assert!(modes.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(modes[0] >= 150.0 && modes[399] <= 18_100.0);
        // Constant density above the knee: as many modes in each kilohertz.
        let count = |low: f64, high: f64| modes.iter().filter(|&&f| f >= low && f < high).count();
        let (a, b) = (count(2_000.0, 6_000.0), count(12_000.0, 16_000.0));
        assert!((a as f64 / b as f64 - 1.0).abs() < 0.1, "{a} against {b}");
        // Fewer below it.
        assert!(count(150.0, 800.0) < count(800.0, 1_450.0));
        // The bands split the modes at their edges.
        let (start, end) = plate.band_range(1);
        assert!(modes[start] >= 1_500.0 && modes[end - 1] < 4_000.0);
        // Configured again the same, nothing changes; tuned, everything
        // moves.
        let before = modes.to_vec();
        plate.configure(&crash(), 1.0, 1.0, RATE);
        assert_eq!(plate.frequencies(), &before[..]);
        plate.configure(&crash(), 1.5, 1.0, RATE);
        assert!((plate.frequencies()[10] / before[10] - 1.5).abs() < 1e-9);
    }

    #[test]
    fn a_plate_rings_with_each_mode_decaying_as_specified() {
        let spec = PlateSpec {
            modes: 8,
            split: 0.0,
            low_hz: 400.0,
            knee_hz: 400.0,
            high_hz: 8_000.0,
            ..crash()
        };
        let mut plate = Plate::new();
        plate.configure(&spec, 1.0, 1.0, RATE);
        let strike = PlateStrike {
            level: 0.5,
            tilt: 0.0,
            low_cut: 0.0,
            bands: [1.0; PLATE_BANDS],
            hold: [0.1; PLATE_BANDS],
            fill: [0.0; PLATE_BANDS],
            gain: [0.0; PLATE_BANDS],
        };
        plate.strike(&strike, &mut Noise::for_note(1, 1.0, 1), RATE);
        let signal: Vec<f64> = (0..(3.0 * RATE) as usize)
            .map(|n| plate.next(if n == 0 { 1.0 } else { 0.0 }))
            .collect();
        let modes = plate.frequencies().to_vec();
        // Modes far enough from their neighbours to be measured alone.
        for (i, &hertz) in modes.iter().enumerate() {
            let gap = modes
                .iter()
                .enumerate()
                .filter(|&(k, &f)| k != i && (f - hertz).abs() > 1e-6)
                .map(|(_, &f)| (f - hertz).abs())
                .fold(f64::INFINITY, f64::min);
            if gap < 60.0 {
                continue;
            }
            let expected = spec.t60_at(hertz);
            let decay = t60_at(&signal, hertz, 0.05, (0.9 * expected).min(2.9));
            assert!((decay / expected - 1.0).abs() < 0.03, "{hertz}: {decay}");
        }
        while plate.is_sounding() {
            plate.next(0.0);
        }
        assert_eq!(plate.next(0.0), 0.0);
    }
}
