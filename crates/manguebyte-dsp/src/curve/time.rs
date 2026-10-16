//! Shapes in time: envelopes, modulation cycles, echoes and decays.

use core::f64::consts::TAU;

use super::{Axis, Curve, CurveKind, CurveSpec, MIN_DB, Point, Scale, Unit};
use crate::numeric::limit;
use crate::{AdsrParams, LfoShape, chorus, phaser, reverb};

/// A duration in seconds that is safe to add up: zero for a negative one
/// or for not-a-number, and no more than a day.
fn duration(seconds: f64) -> f64 {
    limit(seconds, 0.0, 86_400.0)
}

/// The shape of an [`Adsr`](crate::Adsr) envelope for a note held for a
/// given time: its corners, from the note's start to the end of its
/// release.
///
/// The corners are where the straight lines of the envelope meet: the
/// start at silence, the end of the attack at full level, the end of the
/// decay at the sustain level, the release of the note and the end of the
/// release. A note released before the decay ends releases from where it
/// was, over the whole release time, as the envelope does; its shape then
/// has fewer corners.
///
/// The envelope moves in whole samples. Run at a sample rate `fs`, the
/// `n`-th sample after the note starts (counting from 0) is the shape's
/// level at `(n + 1) / fs` seconds, give or take one step of the stage it
/// is in: a stage whose length is not a whole number of samples ends on the
/// sample after, and one shorter than a sample is over at once.
///
/// ```
/// use manguebyte_dsp::AdsrParams;
/// use manguebyte_dsp::curve::{Curve, Point};
///
/// let params = AdsrParams { attack: 0.01, decay: 0.1, sustain: 0.5, release: 0.2 };
/// let shape = params.shape(0.5);
/// assert_eq!(
///     shape.points(),
///     &[
///         Point::new(0.0, 0.0),
///         Point::new(0.01, 1.0),
///         Point::new(0.11, 0.5),
///         Point::new(0.5, 0.5),
///         Point::new(0.7, 0.0),
///     ]
/// );
/// assert!((shape.value_at(0.06) - 0.75).abs() < 1e-12);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnvelopeShape {
    points: [Point; 5],
    len: usize,
}

impl EnvelopeShape {
    /// The shape of an envelope with `params` for a note held for
    /// `gate_seconds`. Durations that are negative or not a number count as
    /// zero, and the sustain level is limited to `0.0..=1.0`.
    pub fn new(params: AdsrParams, gate_seconds: f64) -> Self {
        let attack = duration(params.attack);
        let decay = duration(params.decay);
        let release = duration(params.release);
        let sustain = limit(params.sustain, 0.0, 1.0);
        let gate = duration(gate_seconds);

        let mut shape = Self {
            points: [Point::default(); 5],
            len: 1,
        };
        let mut push = |point: Point| {
            shape.points[shape.len] = point;
            shape.len += 1;
        };
        // The last corner before the release, and the level the release
        // starts from.
        let (corner, level) = if gate >= attack + decay {
            push(Point::new(attack, 1.0));
            push(Point::new(attack + decay, sustain));
            (attack + decay, sustain)
        } else if gate >= attack {
            // Here the decay is longer than zero.
            push(Point::new(attack, 1.0));
            (attack, 1.0 - (1.0 - sustain) * (gate - attack) / decay)
        } else {
            // And here the attack.
            (0.0, gate / attack)
        };
        if gate > corner {
            push(Point::new(gate, level));
        }
        push(Point::new(gate + release, 0.0));
        shape
    }

    /// The corners, in time order: time in seconds from the start of the
    /// note, and level from 0 to 1.
    pub fn points(&self) -> &[Point] {
        &self.points[..self.len]
    }

    /// When the release ends, in seconds from the start of the note.
    pub fn duration(&self) -> f64 {
        self.points[self.len - 1].x
    }
}

impl Curve for EnvelopeShape {
    /// Seconds from the start of the note, to the end of the release,
    /// against the level from 0 to 1.
    fn spec(&self) -> CurveSpec {
        let end = self.duration();
        CurveSpec {
            kind: CurveKind::Envelope,
            x: Axis::new(
                Unit::Seconds,
                Scale::Linear,
                0.0,
                if end > 0.0 { end } else { 1e-3 },
            ),
            y: Axis::new(Unit::Value, Scale::Linear, 0.0, 1.0),
        }
    }

    /// The level at `seconds` after the start of the note: on the straight
    /// line between the two corners around it, and silence before the note
    /// and after its release.
    fn value_at(&self, seconds: f64) -> f64 {
        if seconds.is_nan() || seconds < 0.0 {
            return 0.0;
        }
        for pair in self.points().windows(2) {
            let (from, to) = (pair[0], pair[1]);
            if seconds <= to.x {
                let span = to.x - from.x;
                return if span > 0.0 {
                    from.y + (to.y - from.y) * ((seconds - from.x) / span)
                } else {
                    to.y
                };
            }
        }
        0.0
    }
}

/// One cycle of an [`Lfo`](crate::Lfo): its value from -1 to 1 against the
/// position in the cycle, from 0 to 1, which is where
/// [`Lfo::set_phase`](crate::Lfo::set_phase) puts it.
///
/// A sample-and-hold has no fixed shape: it draws a new random level at the
/// start of each cycle. Its cycle is drawn as the level it holds, which is
/// the oscillator's own for a cycle taken from
/// [`Lfo::cycle`](crate::Lfo::cycle), and 0, the middle of the range, for
/// one made with [`new`](Self::new).
///
/// ```
/// use manguebyte_dsp::LfoShape;
/// use manguebyte_dsp::curve::{Curve, LfoCycle};
///
/// let cycle = LfoCycle::new(LfoShape::Triangle);
/// assert_eq!(cycle.value_at(0.0), 1.0);
/// assert_eq!(cycle.value_at(0.5), -1.0);
/// assert_eq!(cycle.value_at(1.25), 0.0);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LfoCycle {
    shape: LfoShape,
    held: f64,
}

impl LfoCycle {
    /// One cycle of `shape`.
    pub fn new(shape: LfoShape) -> Self {
        Self::holding(shape, 0.0)
    }

    /// One cycle of `shape`, which for a sample-and-hold is `held` all
    /// along.
    pub(crate) fn holding(shape: LfoShape, held: f64) -> Self {
        Self { shape, held }
    }

    /// The shape.
    pub fn shape(&self) -> LfoShape {
        self.shape
    }
}

impl Curve for LfoCycle {
    /// One cycle against the value from -1 to 1.
    fn spec(&self) -> CurveSpec {
        CurveSpec {
            kind: CurveKind::Modulation,
            x: Axis::new(Unit::Cycles, Scale::Linear, 0.0, 1.0),
            y: Axis::new(Unit::Value, Scale::Linear, -1.0, 1.0),
        }
    }

    /// The value at `position` in the cycle; the cycle repeats either side
    /// of `0.0..1.0`.
    fn value_at(&self, position: f64) -> f64 {
        let phase = if position.is_finite() {
            position.rem_euclid(1.0)
        } else {
            0.0
        };
        match self.shape {
            LfoShape::SampleAndHold => self.held,
            shape => shape.value_at(phase),
        }
    }
}

/// How the delay of one voice of a [`Chorus`](crate::Chorus) sweeps over
/// one cycle of its modulation, in milliseconds.
///
/// The delay is `centre + depth·sin(2π·(position + offset))`, where the
/// offset is the voice's place in the cycle plus its channel's spread, and
/// it never comes closer than two samples behind the input, where the
/// chorus stops the sweep.
///
/// ```
/// use manguebyte_dsp::curve::{ChorusSweep, Curve};
///
/// let sweep = ChorusSweep::new(15.0, 3.0, 0.5, 48_000.0);
/// assert!((sweep.value_at(0.25) - 18.0).abs() < 1e-9);
/// assert!((sweep.value_at(0.75) - 12.0).abs() < 1e-9);
/// assert_eq!(sweep.period_seconds(), 2.0);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChorusSweep {
    rate_hz: f64,
    /// Centre delay and depth, in samples.
    centre: f64,
    depth: f64,
    /// How far ahead of the cycle the channel and the voice are, in
    /// cycles.
    ahead: f64,
    place: f64,
    sample_rate: f64,
}

impl ChorusSweep {
    /// The sweep of a chorus centred on `centre_ms`, sweeping `depth_ms`
    /// either side, `rate_hz` times a second, at `sample_rate`, with the
    /// limits the chorus applies: the centre to `0.0..=100.0` ms, the depth
    /// to `0.0..=50.0` ms, the rate to `0.0..=100.0` Hz. It is that of the
    /// first voice of the first channel; [`with_offset`](Self::with_offset)
    /// moves it along the cycle.
    pub fn new(centre_ms: f64, depth_ms: f64, rate_hz: f64, sample_rate: f64) -> Self {
        let (centre_ms, depth_ms, rate_hz) = chorus::limited(centre_ms, depth_ms, rate_hz);
        Self::from_samples(
            rate_hz,
            centre_ms * sample_rate / 1_000.0,
            depth_ms * sample_rate / 1_000.0,
            0.0,
            0.0,
            sample_rate,
        )
    }

    /// The sweep of a chorus whose centre delay and depth are `centre` and
    /// `depth` samples, for a channel `ahead` cycles ahead of the first
    /// and a voice at `place` in the cycle.
    pub(crate) fn from_samples(
        rate_hz: f64,
        centre: f64,
        depth: f64,
        ahead: f64,
        place: f64,
        sample_rate: f64,
    ) -> Self {
        Self {
            rate_hz,
            centre,
            depth,
            ahead,
            place,
            sample_rate,
        }
    }

    /// The same sweep, `cycles` further along: the place of another voice
    /// or the spread of another channel.
    #[must_use]
    pub fn with_offset(self, cycles: f64) -> Self {
        Self {
            place: self.place + cycles,
            ..self
        }
    }

    /// How long one cycle lasts, in seconds; infinite for a sweep that
    /// stands still.
    pub fn period_seconds(&self) -> f64 {
        1.0 / self.rate_hz
    }

    /// The delay in samples at `position` in the cycle.
    fn delay_samples(&self, position: f64) -> f64 {
        let sweep = (TAU * ((position + self.ahead) + self.place)).sin();
        let longest = chorus::longest_delay(self.sample_rate) as f64;
        limit(self.centre + self.depth * sweep, chorus::MIN_DELAY, longest)
    }
}

impl Curve for ChorusSweep {
    /// One cycle against the delay in milliseconds, from the shortest to
    /// the longest the sweep reaches.
    fn spec(&self) -> CurveSpec {
        let to_ms = 1_000.0 / self.sample_rate;
        let lowest = limit(self.centre - self.depth, chorus::MIN_DELAY, f64::MAX) * to_ms;
        let highest = (self.centre + self.depth).max(chorus::MIN_DELAY) * to_ms;
        let (low, high) = if highest > lowest {
            (lowest, highest)
        } else {
            (0.0, 2.0 * highest.max(1.0))
        };
        CurveSpec {
            kind: CurveKind::Modulation,
            x: Axis::new(Unit::Cycles, Scale::Linear, 0.0, 1.0),
            y: Axis::new(Unit::Milliseconds, Scale::Linear, low, high),
        }
    }

    fn value_at(&self, position: f64) -> f64 {
        let position = if position.is_finite() { position } else { 0.0 };
        let ms = self.delay_samples(position) * 1_000.0 / self.sample_rate;
        if ms.is_finite() { ms } else { 0.0 }
    }
}

/// How the frequency of the all-pass stages of a [`Phaser`](crate::Phaser)
/// sweeps over one cycle of its modulation, in hertz:
/// `centre·2^(depth·sin(2π·(position + offset)))`, between 10 Hz and 0.49 of
/// the sample rate.
///
/// ```
/// use manguebyte_dsp::curve::{Curve, PhaserSweep};
///
/// // Two octaves either side of 1 kHz.
/// let sweep = PhaserSweep::new(1_000.0, 2.0, 0.5, 48_000.0);
/// assert!((sweep.value_at(0.25) - 4_000.0).abs() < 1e-9);
/// assert!((sweep.value_at(0.75) - 250.0).abs() < 1e-9);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhaserSweep {
    rate_hz: f64,
    /// The centre as the base-two logarithm of hertz.
    centre: f64,
    depth: f64,
    ahead: f64,
    sample_rate: f64,
}

impl PhaserSweep {
    /// The sweep of a phaser centred on `centre_hz`, sweeping
    /// `depth_octaves` either side, `rate_hz` times a second, at
    /// `sample_rate`, with the limits the phaser applies: the centre to
    /// `10.0..=20000.0` Hz, the depth to `0.0..=6.0` octaves, the rate to
    /// `0.0..=100.0` Hz. It is that of the first channel;
    /// [`with_offset`](Self::with_offset) moves it along the cycle.
    pub fn new(centre_hz: f64, depth_octaves: f64, rate_hz: f64, sample_rate: f64) -> Self {
        let (centre_hz, depth_octaves, rate_hz) =
            phaser::limited(centre_hz, depth_octaves, rate_hz);
        Self::from_parts(rate_hz, centre_hz.log2(), depth_octaves, 0.0, sample_rate)
    }

    /// The sweep around `centre` (base-two logarithm of hertz) by `depth`
    /// octaves, for a channel `ahead` cycles ahead of the first.
    pub(crate) fn from_parts(
        rate_hz: f64,
        centre: f64,
        depth: f64,
        ahead: f64,
        sample_rate: f64,
    ) -> Self {
        Self {
            rate_hz,
            centre,
            depth,
            ahead,
            sample_rate,
        }
    }

    /// The same sweep, `cycles` further along: the spread of another
    /// channel.
    #[must_use]
    pub fn with_offset(self, cycles: f64) -> Self {
        Self {
            ahead: self.ahead + cycles,
            ..self
        }
    }

    /// How long one cycle lasts, in seconds; infinite for a sweep that
    /// stands still.
    pub fn period_seconds(&self) -> f64 {
        1.0 / self.rate_hz
    }

    /// The lowest and highest frequency the phaser lets its stages reach.
    fn limits(&self) -> (f64, f64) {
        let highest = 0.49 * self.sample_rate;
        (phaser::MIN_HZ.min(highest), highest)
    }
}

impl Curve for PhaserSweep {
    /// One cycle against hertz on a logarithmic scale, from the lowest to
    /// the highest frequency the sweep reaches.
    fn spec(&self) -> CurveSpec {
        let (lowest, highest) = self.limits();
        let low = limit((self.centre - self.depth).exp2(), lowest, highest);
        let high = limit((self.centre + self.depth).exp2(), lowest, highest);
        let (low, high) = if high > low {
            (low, high)
        } else {
            (0.5 * low, 2.0 * high)
        };
        CurveSpec {
            kind: CurveKind::Modulation,
            x: Axis::new(Unit::Cycles, Scale::Linear, 0.0, 1.0),
            y: Axis::new(Unit::Hertz, Scale::Logarithmic, low, high),
        }
    }

    fn value_at(&self, position: f64) -> f64 {
        let position = if position.is_finite() { position } else { 0.0 };
        let (lowest, highest) = self.limits();
        let sweep = (TAU * (position + self.ahead)).sin();
        limit((self.centre + self.depth * sweep).exp2(), lowest, highest)
    }
}

/// The echoes of a [`Delay`](crate::Delay): when each one comes and how
/// loud it is, drawn as stems.
///
/// Tap 0 is the dry signal, at time zero with the dry gain `1 - mix`. Echo
/// `k` comes `k` delay times later with the gain `mix·feedback^(k - 1)`.
/// The delay time is the one the delay line reads at: at least a sample,
/// rounded to a whole sample without interpolation. With a delay between
/// two samples, an interpolating read spreads each echo over the samples
/// around it (and the all-pass of [`DelayInterpolation::Thiran`](crate::DelayInterpolation::Thiran)
/// rings a little); the gain is then that of the echo as a whole at low
/// frequencies, where every interpolation passes it at its full level.
///
/// ```
/// use manguebyte_dsp::curve::{DelayTaps, Point};
///
/// let taps = DelayTaps::new(0.25, 0.5, 1.0, 48_000.0);
/// let mut echoes = [Point::default(); 8];
/// assert_eq!(taps.fill(&mut echoes), 8);
/// assert_eq!(echoes[0], Point::new(0.0, 0.0));
/// assert_eq!(echoes[1], Point::new(0.25, 1.0));
/// assert_eq!(echoes[3], Point::new(0.75, 0.25));
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DelayTaps {
    /// The delay, in samples.
    delay: f64,
    feedback: f64,
    mix: f64,
    dry: f64,
    sample_rate: f64,
}

/// Echoes quieter than this are not drawn: -120 dB.
const QUIETEST_TAP: f64 = 1e-6;

impl DelayTaps {
    /// The echoes of a delay set to `time_seconds`, `feedback` and `mix`,
    /// at `sample_rate`, with the limits the delay applies: the time to at
    /// least one sample, the feedback to `0.0..=0.99` and the mix to
    /// `0.0..=1.0` (not-a-number as zero).
    pub fn new(time_seconds: f64, feedback: f64, mix: f64, sample_rate: f64) -> Self {
        let mix = limit(mix, 0.0, 1.0);
        Self::from_parts(
            limit(time_seconds * sample_rate, 1.0, f64::MAX),
            limit(feedback, 0.0, 0.99),
            mix,
            1.0 - mix,
            sample_rate,
        )
    }

    /// The echoes of a delay of `delay` samples.
    pub(crate) fn from_parts(
        delay: f64,
        feedback: f64,
        mix: f64,
        dry: f64,
        sample_rate: f64,
    ) -> Self {
        Self {
            delay,
            feedback,
            mix,
            dry,
            sample_rate,
        }
    }

    /// The time between two echoes, in seconds.
    pub fn delay_seconds(&self) -> f64 {
        self.delay / self.sample_rate
    }

    /// Tap `index`: its time in seconds and its gain. Tap 0 is the dry
    /// signal.
    pub fn tap(&self, index: usize) -> Point {
        let gain = if index == 0 {
            self.dry
        } else {
            let repeats = i32::try_from(index - 1).unwrap_or(i32::MAX);
            self.mix * self.feedback.powi(repeats)
        };
        Point::new(index as f64 * self.delay_seconds(), gain)
    }

    /// Writes the dry tap and the echoes, in time order, to `taps`, and
    /// returns how many it wrote: until `taps` is full or the echoes are
    /// more than 120 dB down. Without feedback there is one echo.
    pub fn fill(&self, taps: &mut [Point]) -> usize {
        let mut written = 0;
        for (index, slot) in taps.iter_mut().enumerate() {
            let tap = self.tap(index);
            if index > 0 && (tap.y < QUIETEST_TAP || (index > 1 && self.feedback == 0.0)) {
                break;
            }
            *slot = tap;
            written += 1;
        }
        written
    }

    /// Time against gain, from the dry tap to the last echo more than
    /// 120 dB down, or the 32nd, whichever comes first.
    pub fn spec(&self) -> CurveSpec {
        let mut last = 1;
        while last < 32 {
            let next = self.tap(last + 1);
            if next.y < QUIETEST_TAP || self.feedback == 0.0 {
                break;
            }
            last += 1;
        }
        let end = self.tap(last).x;
        CurveSpec {
            kind: CurveKind::Taps,
            x: Axis::new(
                Unit::Seconds,
                Scale::Linear,
                0.0,
                if end > 0.0 && end.is_finite() {
                    end
                } else {
                    1.0
                },
            ),
            y: Axis::new(Unit::Value, Scale::Linear, 0.0, 1.0),
        }
    }
}

/// How the tail of a [`Reverb`](crate::Reverb) dies away: the time it
/// takes to fall by 60 dB (RT60) at each frequency.
///
/// # The model
///
/// The reverb is a feedback delay network (J.-M. Jot and A. Chaigne,
/// "Digital delay networks for designing artificial reverberators", 90th
/// AES Convention, 1991): eight delay lines of `L_i` samples, each followed
/// by a one-pole low-pass `H(f) = (1 - p) / (1 - p·z⁻¹)` (`p` is 0.75 times
/// the damping) and a gain `g_i` that takes exactly the decay time to bring
/// a trip round the line 60 dB down, and mixed by the Householder matrix
/// `M = I - J/4`, which neither adds energy nor takes it away. At 0 Hz,
/// where `|H| = 1`, every line loses the same `60 / decay` dB per second,
/// so every resonance of the network decays at that rate and the RT60 is
/// the decay time, exactly. Above it the low-pass takes `-20·log10|H(f)|`
/// decibels per trip as well, which is more per second in a short line than
/// in a long one, and the network decays at a rate between those of its
/// lines.
///
/// Which rate is worked out from the flow of energy: what enters line `i`
/// comes out `L_i` samples later scaled by `a_i² = g_i²·|H(f)|²`, and is
/// spread over the lines by the squares of the entries of `M`, `M² = I/2 +
/// J/16`. Energy decaying as `e^(-s·n)` keeps its distribution over the
/// lines exactly when the matrix `M²·diag(a_i²·e^(s·L_i))` has 1 as its
/// largest eigenvalue, which for this `M²` comes down to one equation,
///
/// ```text
/// Σ c_i / (1 - c_i/2) = 16,   c_i = a_i²·e^(s·L_i),
/// ```
///
/// solved for `s` by bisection. It gives the RT60 at 0 Hz exactly, and is
/// computed from the settings alone, without allocating.
///
/// # How well it matches
///
/// The model was checked against the reverb itself, rendered offline: the
/// impulse responses of both outputs, filtered into octave bands at 125
/// and 500 Hz and third-octave bands at 2 and 8 kHz, their energies
/// integrated backwards (M. R. Schroeder, "New method of measuring
/// reverberation time", JASA 37, 1965) and the slope fitted from 5 to
/// 25 dB down, against the same fit to this model's decay integrated over
/// the same band. Over decay times of 0.5, 2 and 6 s, damping of 0, 0.5 and
/// 1, sizes of 0, 0.5 and 1, at 44.1 and 96 kHz:
///
/// - without damping, where the model is exact, the measurements were
///   within 2.4 % of it for decays of 2 s and more, and within 8 % for
///   0.5 s, which is how far the few resonances in a band let the
///   measurement of a short tail stray;
/// - with damping, for decays of 2 s and more, within 3 % up to 500 Hz,
///   5.3 % at 2 kHz, and at 8 kHz 1.5 % with half the damping and 11 % with
///   all of it; for 0.5 s, within 9 %, but 19 % at full damping at 8 kHz.
///
/// The largest differences are where the tail is shortest, below 0.1 s:
/// the measured one is then the longer, as short a tail being drawn out by
/// the ringing of the input diffusers, which lasts about 0.07 s and which
/// the model leaves out. A model that spreads the energy over the lines in
/// proportion to their lengths instead (the decay rate `60 / decay +
/// 8·fs·(-20·log10|H(f)|) / ΣL_i`) was off by up to 41 % there.
///
/// ```
/// use manguebyte_dsp::curve::{Curve, ReverbDecay};
///
/// let decay = ReverbDecay::new(2.5, 0.4, 0.5, 48_000.0);
/// // The decay time holds at low frequencies; the damping shortens the top.
/// assert!((decay.rt60_at(0.0) - 2.5).abs() < 1e-9);
/// assert!(decay.rt60_at(10_000.0) < 1.0);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReverbDecay {
    decay_seconds: f64,
    /// The pole of the damping filter.
    pole: f64,
    /// The lengths of the delay lines, in samples.
    lengths: [f64; reverb::LINES],
    sample_rate: f64,
}

/// How many halvings [`ReverbDecay::db_per_second_at`] takes to find the
/// decay rate: from the whole range it can lie in to its last digits.
const BISECTIONS: usize = 64;

impl ReverbDecay {
    /// The decay of a reverb set to `decay_seconds`, `damping` and `size`,
    /// at `sample_rate`, with the limits the reverb applies: the decay to
    /// `0.05..=120.0` seconds, the damping and the size to `0.0..=1.0`
    /// (not-a-number as the lowest).
    pub fn new(decay_seconds: f64, damping: f64, size: f64, sample_rate: f64) -> Self {
        Self::from_parts(
            limit(decay_seconds, 0.05, 120.0),
            limit(damping, 0.0, 1.0),
            limit(size, 0.0, 1.0),
            sample_rate,
        )
    }

    /// The decay of a reverb whose settings are already limited.
    pub(crate) fn from_parts(
        decay_seconds: f64,
        damping: f64,
        size: f64,
        sample_rate: f64,
    ) -> Self {
        Self {
            decay_seconds,
            pole: reverb::damping_pole(damping),
            lengths: reverb::line_lengths(size, sample_rate).map(|len| len as f64),
            sample_rate,
        }
    }

    /// The decay time the reverb is set to, in seconds: its RT60 at 0 Hz.
    pub fn decay_seconds(&self) -> f64 {
        self.decay_seconds
    }

    /// How fast the tail falls at `frequency` hertz, in decibels per
    /// second. See [the model](Self#the-model).
    pub fn db_per_second_at(&self, frequency: f64) -> f64 {
        let frequency = limit(frequency, 0.0, 0.5 * self.sample_rate);
        let omega = TAU * frequency / self.sample_rate;
        let p = self.pole;
        // |H|² = (1 - p)² / |1 - p·e^(-jω)|², with the denominator written
        // as (1 - p)² + 4p·sin²(ω/2), which keeps its digits at low
        // frequencies.
        let half = (0.5 * omega).sin();
        let lowpass = (1.0 - p) * (1.0 - p) / ((1.0 - p) * (1.0 - p) + 4.0 * p * half * half);
        // The energy kept by a trip round each line: the line's gain, which
        // takes 60 dB in the decay time, squared, and the low-pass's.
        let trip =
            |length: f64| 10f64.powf(-6.0 * length / (self.sample_rate * self.decay_seconds));
        let kept = self.lengths.map(|length| trip(length) * lowpass);
        let balance = |s: f64| -> f64 {
            kept.iter()
                .zip(&self.lengths)
                .map(|(&a, &length)| {
                    let c = a * (s * length).exp();
                    c / (1.0 - 0.5 * c)
                })
                .sum()
        };
        // Without loss, s = 0 balances; the rate cannot be so fast that a
        // line would give back twice what it got.
        let mut low = 0.0;
        let mut high = kept
            .iter()
            .zip(&self.lengths)
            .map(|(&a, &length)| (2.0 / a).ln() / length)
            .fold(f64::INFINITY, f64::min);
        for _ in 0..BISECTIONS {
            let middle = 0.5 * (low + high);
            if balance(middle) < 16.0 {
                low = middle;
            } else {
                high = middle;
            }
        }
        // Nepers of energy per sample to decibels per second.
        10.0 * (0.5 * (low + high)) * self.sample_rate / core::f64::consts::LN_10
    }

    /// The time the tail takes to fall by 60 dB at `frequency` hertz, in
    /// seconds.
    pub fn rt60_at(&self, frequency: f64) -> f64 {
        let rate = self.db_per_second_at(frequency);
        if rate > 0.0 && rate.is_finite() {
            60.0 / rate
        } else {
            self.decay_seconds
        }
    }

    /// How the tail dies away at `frequency` hertz, as level against time.
    pub fn energy_at(&self, frequency: f64) -> ReverbEnergy {
        ReverbEnergy {
            db_per_second: self.db_per_second_at(frequency),
            decay_seconds: self.decay_seconds,
        }
    }
}

impl Curve for ReverbDecay {
    /// The frequencies of [`Axis::frequency`] against the RT60 in seconds,
    /// from zero to a little above the decay time.
    fn spec(&self) -> CurveSpec {
        CurveSpec {
            kind: CurveKind::DecayTime,
            x: Axis::frequency(self.sample_rate),
            y: Axis::new(Unit::Seconds, Scale::Linear, 0.0, 1.2 * self.decay_seconds),
        }
    }

    fn value_at(&self, frequency: f64) -> f64 {
        self.rt60_at(frequency)
    }
}

/// How the tail of a [`Reverb`](crate::Reverb) dies away at one frequency:
/// its level against the time since it began, falling in a straight line
/// of decibels at the rate of [`ReverbDecay::db_per_second_at`].
///
/// It is the slope the energy of the tail follows on average, not the
/// detail of one impulse response, which flutters around it, and it starts
/// at 0 dB: the level of the tail once it has built up.
///
/// ```
/// use manguebyte_dsp::curve::{Curve, ReverbDecay};
///
/// let energy = ReverbDecay::new(2.0, 0.0, 0.5, 48_000.0).energy_at(100.0);
/// assert!((energy.value_at(1.0) + 30.0).abs() < 0.01);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReverbEnergy {
    db_per_second: f64,
    decay_seconds: f64,
}

impl Curve for ReverbEnergy {
    /// Seconds, to a little beyond the decay time, against decibels from
    /// -72 to 0.
    fn spec(&self) -> CurveSpec {
        CurveSpec {
            kind: CurveKind::Decay,
            x: Axis::new(Unit::Seconds, Scale::Linear, 0.0, 1.2 * self.decay_seconds),
            y: Axis::new(Unit::Decibels, Scale::Linear, -72.0, 0.0),
        }
    }

    /// The level `seconds` after the tail began, in decibels; 0 dB before.
    fn value_at(&self, seconds: f64) -> f64 {
        let seconds = limit(seconds, 0.0, f64::MAX);
        let db = -self.db_per_second * seconds;
        if db >= MIN_DB { db.min(0.0) } else { MIN_DB }
    }
}

#[cfg(test)]
mod tests {
    use manguebyte_core::AudioBuffer;

    use super::*;
    use crate::curve::FrequencyResponse;
    use crate::iir::{Band, Design};
    use crate::testing::{gain_at, run_in_blocks};
    use crate::{
        Adsr, Chorus, Delay, DelayInterpolation, IirFilter, Lfo, Phaser, ProcessSpec, Processor,
        Reverb,
    };

    fn spec(sample_rate: f64, channels: usize) -> ProcessSpec {
        ProcessSpec {
            sample_rate,
            max_block_size: 512,
            num_channels: channels,
        }
    }

    #[test]
    fn an_envelope_follows_its_shape_sample_by_sample() {
        let rate = 48_000.0;
        let params = |attack, decay, sustain, release| AdsrParams {
            attack,
            decay,
            sustain,
            release,
        };
        let cases = [
            // Through every stage.
            (params(0.01, 0.1, 0.5, 0.2), 0.5),
            // Released during the decay, with stages a fraction of a
            // sample long.
            (params(0.0123, 0.0456, 0.3, 0.0789), 0.03),
            // Released during the attack.
            (params(0.05, 0.1, 0.8, 0.1), 0.02),
            // Every stage at once.
            (params(0.0, 0.0, 1.0, 0.0), 0.01),
            (params(0.002, 0.0, 0.6, 0.03), 0.1),
        ];
        for (params, gate) in cases {
            let shape = params.shape(gate);
            let mut adsr = Adsr::new(params);
            assert_eq!(adsr.shape(gate), shape);
            adsr.set_sample_rate(rate);
            adsr.note_on();
            let step = |seconds: f64| {
                let samples = seconds * rate;
                if samples < 1.0 { 1.0 } else { 1.0 / samples }
            };
            let tolerance = step(params.attack)
                .max(step(params.decay))
                .max(step(params.release))
                + 1e-9;
            let gate_samples = (gate * rate).round() as usize;
            let end = (shape.duration() * rate) as usize + 10;
            for n in 0..end {
                if n == gate_samples {
                    adsr.note_off();
                }
                let level = adsr.next_sample();
                let expected = shape.value_at((n + 1) as f64 / rate);
                assert!(
                    (level - expected).abs() <= tolerance,
                    "{params:?}, sample {n}: {level} against {expected}"
                );
            }
            assert!(!adsr.is_active());
        }
    }

    #[test]
    fn an_envelope_shape_has_the_corners_it_should() {
        let params = AdsrParams {
            attack: 0.1,
            decay: 0.2,
            sustain: 0.5,
            release: 0.4,
        };
        let corners = |gate: f64| params.shape(gate).points().to_vec();
        // Released as the decay ends: no separate corner for the release.
        let end = params.attack + params.decay;
        assert_eq!(
            corners(end),
            [
                Point::new(0.0, 0.0),
                Point::new(0.1, 1.0),
                Point::new(end, 0.5),
                Point::new(end + 0.4, 0.0)
            ]
        );
        // Released during the sustain.
        assert_eq!(corners(0.5)[3], Point::new(0.5, 0.5));
        assert_eq!(corners(0.5).len(), 5);
        // Released half-way up the attack, then over the whole release.
        assert_eq!(
            corners(0.05),
            [
                Point::new(0.0, 0.0),
                Point::new(0.05, 0.5),
                Point::new(0.45, 0.0)
            ]
        );
        // Released at once: the release starts from silence.
        assert_eq!(corners(0.0), [Point::new(0.0, 0.0), Point::new(0.4, 0.0)]);
        let shape = params.shape(1.0);
        assert_eq!(shape.duration(), 1.4);
        assert_eq!(shape.value_at(-1.0), 0.0);
        assert_eq!(shape.value_at(2.0), 0.0);
        assert_eq!(shape.value_at(f64::NAN), 0.0);
        let spec = shape.spec();
        assert_eq!(
            (spec.kind, spec.x.max, spec.y.max),
            (CurveKind::Envelope, 1.4, 1.0)
        );
        // Garbage settings give a shape all the same.
        let broken = AdsrParams {
            attack: f64::NAN,
            decay: -1.0,
            sustain: f64::INFINITY,
            release: f64::INFINITY,
        };
        let shape = broken.shape(f64::NAN);
        assert!(
            shape
                .points()
                .iter()
                .all(|p| p.x.is_finite() && p.y.is_finite())
        );
        assert!(shape.value_at(1.0).is_finite());
    }

    #[test]
    fn an_lfo_runs_through_the_cycle_it_reports() {
        let shapes = [
            LfoShape::Sine,
            LfoShape::Triangle,
            LfoShape::SawUp,
            LfoShape::SawDown,
            LfoShape::Square,
        ];
        for shape in shapes {
            let mut lfo = Lfo::new(shape, 3.0);
            lfo.set_sample_rate(1_000.0);
            let cycle = lfo.cycle();
            assert_eq!(cycle, LfoCycle::new(shape));
            let mut phase = 0.0;
            for _ in 0..2_000 {
                assert_eq!(lfo.next_value(), cycle.value_at(phase));
                phase = (phase + 3.0 / 1_000.0).rem_euclid(1.0);
            }
        }
        // A sample-and-hold is drawn at the level it holds, cycle after
        // cycle.
        let mut lfo = Lfo::new(LfoShape::SampleAndHold, 10.0);
        lfo.set_sample_rate(1_000.0);
        let mut levels = Vec::new();
        for _ in 0..10 {
            let cycle = lfo.cycle();
            for _ in 0..100 {
                assert_eq!(lfo.value(), cycle.value_at(0.3));
                lfo.next_value();
            }
            levels.push(cycle.value_at(0.0));
        }
        assert!(levels.windows(2).all(|pair| pair[0] != pair[1]));
        assert_eq!(LfoCycle::new(LfoShape::SampleAndHold).value_at(0.5), 0.0);
        assert_eq!(LfoCycle::new(LfoShape::Sine).value_at(f64::NAN), 0.0);
    }

    #[test]
    fn a_chorus_voice_reads_where_its_sweep_says() {
        let rate = 48_000.0;
        // A depth larger than the centre, so that the sweep is stopped two
        // samples behind the input for part of the cycle.
        let mut chorus = Chorus::<f64>::new()
            .with_voices(1)
            .with_rate_hz(2.0)
            .with_depth_ms(2.0)
            .with_center_delay_ms(1.0)
            .with_mix(1.0)
            .with_spread(1.0);
        chorus.prepare(&spec(rate, 2));
        let sweeps = [chorus.sweep(0, 0).unwrap(), chorus.sweep(0, 1).unwrap()];
        assert!(chorus.sweep(1, 0).is_none());
        assert_eq!(sweeps[0], ChorusSweep::new(1.0, 2.0, 2.0, rate));
        assert_eq!(sweeps[0].period_seconds(), 0.5);

        // A ramp read at a delay comes out as the ramp less the delay.
        let len = 30_000;
        let mut ramp = AudioBuffer::<f64>::new(2, len);
        for channel in ramp.iter_channels_mut() {
            for (n, sample) in channel.iter_mut().enumerate() {
                *sample = n as f64;
            }
        }
        run_in_blocks(&mut chorus, &mut ramp, 512);
        let increment = 2.0 / rate;
        let mut phase = 0.0;
        let mut worst = 0.0f64;
        for n in 0..len {
            for (channel, sweep) in sweeps.iter().enumerate() {
                // Once the line holds enough of the ramp.
                if n > 1_000 {
                    let delay_ms = (n as f64 - ramp.channel(channel)[n]) * 1_000.0 / rate;
                    worst = worst.max((delay_ms - sweep.value_at(phase)).abs());
                }
            }
            phase += increment;
            if phase >= 1.0 {
                phase -= 1.0;
            }
        }
        assert!(worst < 1e-9, "{worst} ms");
        // The sweep comes down to two samples, where the chorus stops it.
        let lowest = 2.0 * 1_000.0 / rate;
        assert!((sweeps[0].value_at(0.75) - lowest).abs() < 1e-12);
        let spec = sweeps[0].spec();
        assert!((spec.y.min - lowest).abs() < 1e-12 && (spec.y.max - 3.0).abs() < 1e-12);

        // The voices of a chorus sit around the cycle.
        let chorus = Chorus::<f64>::new().with_voices(4);
        let first = chorus.sweep(0, 0).unwrap();
        assert_eq!(chorus.sweep(1, 0).unwrap(), first.with_offset(0.5));
        assert_eq!(chorus.sweep(2, 0).unwrap(), first.with_offset(0.25));
    }

    /// The frequency of the notch of `response` between `low` and `high`
    /// hertz, by golden-section search on its gain.
    fn notch_between(response: &[f64], rate: f64, low: f64, high: f64) -> f64 {
        let ratio = (5f64.sqrt() - 1.0) / 2.0;
        let gain = |log: f64| gain_at(response, log.exp(), rate);
        let (mut a, mut b) = (low.ln(), high.ln());
        while b - a > 1e-10 {
            let c = b - ratio * (b - a);
            let d = a + ratio * (b - a);
            if gain(c) < gain(d) {
                b = d;
            } else {
                a = c;
            }
        }
        (0.5 * (a + b)).exp()
    }

    #[test]
    fn a_phaser_tunes_its_stages_where_its_sweep_says() {
        let rate = 48_000.0;
        for steps in [0, 7_000, 19_000, 30_000] {
            // Two stages and an equal mix: a single notch, at the frequency
            // the stages are tuned to.
            let mut phaser = Phaser::<f64>::new()
                .with_stages(2)
                .with_center_hz(1_000.0)
                .with_depth_octaves(2.0)
                .with_rate_hz(1.0)
                .with_spread(0.5)
                .with_mix(0.5);
            phaser.prepare(&spec(rate, 2));
            let sweeps = [phaser.sweep(0), phaser.sweep(1)];
            assert_eq!(sweeps[0], PhaserSweep::new(1_000.0, 2.0, 1.0, rate));
            // Sweep in silence for a while, then hold the sweep where it
            // is.
            if steps > 0 {
                let mut silence = AudioBuffer::<f64>::new(2, steps);
                run_in_blocks(&mut phaser, &mut silence, 512);
            }
            phaser.set_rate_hz(0.0);
            let mut phase = 0.0;
            for _ in 0..steps {
                phase += 1.0 / rate;
                if phase >= 1.0 {
                    phase -= 1.0;
                }
            }
            let mut impulse = AudioBuffer::<f64>::new(2, 1 << 14);
            impulse.channel_mut(0)[0] = 1.0;
            impulse.channel_mut(1)[0] = 1.0;
            run_in_blocks(&mut phaser, &mut impulse, 512);
            for (channel, sweep) in sweeps.iter().enumerate() {
                let tuned = sweep.value_at(phase);
                let notch = notch_between(impulse.channel(channel), rate, 0.5 * tuned, 2.0 * tuned);
                assert!(
                    (notch / tuned - 1.0).abs() < 1e-6,
                    "after {steps} samples, channel {channel}: {notch} against {tuned}"
                );
            }
        }
    }

    #[test]
    fn a_delay_echoes_when_and_as_loud_as_its_taps_say() {
        let rate = 48_000.0;
        // A whole number of samples: every echo on one sample.
        let mut delay = Delay::<f64>::new(0.1)
            .with_time(0.01)
            .with_feedback(0.5)
            .with_mix(0.7);
        delay.prepare(&spec(rate, 1));
        let taps = delay.taps();
        assert_eq!(taps, DelayTaps::new(0.01, 0.5, 0.7, rate));
        let mut impulse = AudioBuffer::<f64>::new(1, 9_600);
        impulse.channel_mut(0)[0] = 1.0;
        run_in_blocks(&mut delay, &mut impulse, 512);
        let mut points = [Point::default(); 64];
        let count = taps.fill(&mut points);
        assert_eq!(count, 21);
        let mut expected = vec![0.0; 9_600];
        for tap in &points[..count] {
            let n = (tap.x * rate).round() as usize;
            if n < expected.len() {
                expected[n] = tap.y;
            }
        }
        for (n, (&out, &tap)) in impulse.channel(0).iter().zip(&expected).enumerate() {
            assert!((out - tap).abs() < 1e-15, "sample {n}: {out} against {tap}");
        }

        // Between two samples, an echo is spread over the samples around
        // its time, with its gain in all and its time as its centre.
        for interpolation in [
            DelayInterpolation::None,
            DelayInterpolation::Linear,
            DelayInterpolation::Cubic,
            DelayInterpolation::Thiran,
        ] {
            let mut delay = Delay::<f64>::new(0.1)
                .with_time(480.3 / rate)
                .with_feedback(0.6)
                .with_mix(1.0)
                .with_interpolation(interpolation);
            delay.prepare(&spec(rate, 1));
            let taps = delay.taps();
            let mut impulse = AudioBuffer::<f64>::new(1, 2_000);
            impulse.channel_mut(0)[0] = 1.0;
            run_in_blocks(&mut delay, &mut impulse, 512);
            for k in 1..=3 {
                let tap = taps.tap(k);
                let at = (tap.x * rate).round() as usize;
                let window = &impulse.channel(0)[at - 100..at + 100];
                let sum: f64 = window.iter().sum();
                let moment: f64 = window
                    .iter()
                    .enumerate()
                    .map(|(i, &s)| (at - 100 + i) as f64 * s)
                    .sum();
                let centre = moment / sum / rate;
                assert!(
                    (sum - tap.y).abs() < 1e-12,
                    "{interpolation:?}, echo {k}: {sum}"
                );
                assert!(
                    (centre - tap.x).abs() < 1e-12,
                    "{interpolation:?}, echo {k}: {centre}"
                );
            }
        }
    }

    #[test]
    fn the_taps_stop_where_the_echoes_die_away() {
        let rate = 48_000.0;
        let mut points = [Point::default(); 400];
        // Without feedback, one echo.
        assert_eq!(DelayTaps::new(0.25, 0.0, 0.5, rate).fill(&mut points), 2);
        assert_eq!(points[0], Point::new(0.0, 0.5));
        // With the most feedback, about 1,375 echoes before -120 dB: as
        // many as there is room for.
        assert_eq!(DelayTaps::new(0.25, 0.99, 1.0, rate).fill(&mut points), 400);
        // An echo below -120 dB is not drawn.
        let count = DelayTaps::new(0.1, 0.5, 1.0, rate).fill(&mut points);
        assert_eq!(count, 21);
        assert!(points[20].y >= 1e-6 && points[20].y / 2.0 < 1e-6);
        // A delay shorter than a sample reads one sample back.
        let taps = DelayTaps::new(0.0, 0.5, 1.0, rate);
        assert_eq!(taps.delay_seconds(), 1.0 / rate);
        // The axis ends at the last echo drawn, or the 32nd.
        let spec = DelayTaps::new(0.1, 0.5, 1.0, rate).spec();
        assert_eq!((spec.kind, spec.x.max), (CurveKind::Taps, 2.0));
        let spec = DelayTaps::new(0.1, 0.99, 1.0, rate).spec();
        assert!((spec.x.max - 3.2).abs() < 1e-12);
        // Garbage gives numbers.
        let taps = DelayTaps::new(f64::NAN, f64::NAN, f64::NAN, rate);
        let count = taps.fill(&mut points);
        assert!(
            points[..count]
                .iter()
                .all(|p| p.x.is_finite() && p.y.is_finite())
        );
        assert!(taps.spec().x.max.is_finite());
    }

    /// The impulse response of both outputs of `reverb`, fed on both
    /// inputs, rendered offline for `seconds`.
    fn render(reverb: &mut Reverb<f64>, rate: f64, seconds: f64) -> AudioBuffer<f64> {
        reverb.prepare(&spec(rate, 2));
        let mut response = AudioBuffer::<f64>::new(2, (seconds * rate) as usize);
        response.channel_mut(0)[0] = 1.0;
        response.channel_mut(1)[0] = 1.0;
        run_in_blocks(reverb, &mut response, 512);
        response
    }

    /// The RT60 of a decay curve in decibels (from 0 dB down), from the
    /// slope of a straight line fitted to it from 5 to 25 dB down.
    fn fitted_rt60(curve: impl Iterator<Item = (f64, f64)>) -> f64 {
        let points: Vec<(f64, f64)> = curve
            .filter(|&(_, db)| (-25.0..=-5.0).contains(&db))
            .collect();
        assert!(points.len() >= 8, "too few points to fit");
        let count = points.len() as f64;
        let (sx, sy) = points
            .iter()
            .fold((0.0, 0.0), |(x, y), p| (x + p.0, y + p.1));
        let (mx, my) = (sx / count, sy / count);
        let (sxy, sxx) = points.iter().fold((0.0, 0.0), |(xy, xx), p| {
            (xy + (p.0 - mx) * (p.1 - my), xx + (p.0 - mx) * (p.0 - mx))
        });
        -60.0 / (sxy / sxx)
    }

    /// An eighth-order Butterworth band-pass from `low` to `high` hertz.
    fn band(low: f64, high: f64) -> Design {
        Design::butterworth(
            Band::Bandpass {
                low_hz: low,
                high_hz: high,
            },
            4,
        )
    }

    /// The RT60 of `response` from `low` to `high` hertz, measured: both
    /// outputs filtered by [`band`], their energies added and integrated
    /// backwards from the end (M. R. Schroeder, "New method of measuring
    /// reverberation time", JASA 37, 1965), and [`fitted_rt60`].
    fn measured_rt60(response: &AudioBuffer<f64>, rate: f64, low: f64, high: f64) -> f64 {
        let mut filtered = response.clone();
        let mut filter = IirFilter::<f64>::new(band(low, high));
        filter.prepare(&spec(rate, 2));
        run_in_blocks(&mut filter, &mut filtered, 512);
        let len = filtered.num_frames();
        let mut decay: Vec<f64> = (0..len)
            .map(|n| filtered.channel(0)[n].powi(2) + filtered.channel(1)[n].powi(2))
            .collect();
        for n in (0..len - 1).rev() {
            decay[n] += decay[n + 1];
        }
        let top = decay[0];
        fitted_rt60(
            decay
                .iter()
                .enumerate()
                .step_by(16)
                .map(|(n, &energy)| (n as f64 / rate, 10.0 * (energy / top).log10())),
        )
    }

    /// The RT60 [`measured_rt60`] finds in a band if the tail decays as
    /// `decay` says at every frequency: the energy of each frequency,
    /// weighted by the band's filter and decaying at its own rate, added up
    /// and integrated backwards (which for `10^(-r·t/10)` is that over
    /// `r`, give or take a constant), and the same fit. A band whose decay
    /// rate changes across it decays, as a whole, at neither its slowest
    /// nor its fastest rate.
    fn predicted_rt60(decay: &ReverbDecay, rate: f64, low: f64, high: f64, seconds: f64) -> f64 {
        let filter = band(low, high).response(rate);
        let mut hertz = [0.0; 120];
        crate::curve::linear_spaced(0.5 * low, (2.0 * high).min(0.49 * rate), &mut hertz);
        let parts = hertz.map(|f| {
            let weight = 10f64.powf(filter.magnitude_db_at(f) / 10.0);
            (weight, decay.db_per_second_at(f))
        });
        let edc = |t: f64| -> f64 {
            parts
                .iter()
                .map(|&(w, r)| w * 10f64.powf(-r * t / 10.0) / r)
                .sum()
        };
        let top = edc(0.0);
        fitted_rt60(
            (0..2_000)
                .map(|i| seconds * f64::from(i) / 2_000.0)
                .map(|t| (t, 10.0 * (edc(t) / top).log10())),
        )
    }

    #[test]
    fn a_reverb_dies_away_as_its_decay_says() {
        let rate = 48_000.0;
        // Octaves low down, where a third of an octave holds too few of the
        // network's resonances, and the furthest each may stray (see the
        // documentation of `ReverbDecay` for what was measured on a wider
        // grid).
        let bands = [
            (125.0, 0.5, 0.03),
            (500.0, 0.5, 0.03),
            (2_000.0, 1.0 / 6.0, 0.06),
            (8_000.0, 1.0 / 6.0, 0.12),
        ];
        for (decay_seconds, damping, size) in [
            (2.0, 0.0, 0.5),
            (2.0, 0.5, 0.0),
            (2.0, 1.0, 1.0),
            (5.0, 0.7, 0.3),
        ] {
            let mut reverb = Reverb::<f64>::new()
                .with_decay_seconds(decay_seconds)
                .with_damping(damping)
                .with_size(size)
                .with_mix(1.0);
            reverb.prepare(&spec(rate, 2));
            let decay = reverb.decay();
            assert_eq!(decay, ReverbDecay::new(decay_seconds, damping, size, rate));
            let seconds = 0.6 * decay_seconds + 0.3;
            let response = render(&mut reverb, rate, seconds);
            for (frequency, half_width, tolerance) in bands {
                let (low, high) = (
                    frequency * 2f64.powf(-half_width),
                    frequency * 2f64.powf(half_width),
                );
                let measured = measured_rt60(&response, rate, low, high);
                let predicted = predicted_rt60(&decay, rate, low, high, seconds);
                let error = measured / predicted - 1.0;
                assert!(
                    error.abs() < tolerance,
                    "{decay_seconds} s, damping {damping}, size {size}, {frequency} Hz: \
                     measured {measured}, predicted {predicted} ({:+.2} %)",
                    100.0 * error
                );
            }
        }
    }

    #[test]
    fn the_decay_time_holds_wherever_nothing_damps_the_tail() {
        for rate in [44_100.0, 48_000.0, 192_000.0] {
            for decay_seconds in [0.05, 0.7, 3.0, 120.0] {
                for size in [0.0, 0.4, 1.0] {
                    let undamped = ReverbDecay::new(decay_seconds, 0.0, size, rate);
                    let damped = ReverbDecay::new(decay_seconds, 1.0, size, rate);
                    for frequency in [0.0, 100.0, 5_000.0, 0.5 * rate] {
                        let rt60 = undamped.rt60_at(frequency);
                        assert!((rt60 / decay_seconds - 1.0).abs() < 1e-12, "{rt60}");
                    }
                    assert!((damped.rt60_at(0.0) / decay_seconds - 1.0).abs() < 1e-12);
                    // Damping only ever shortens it, more the higher up.
                    let mut previous = decay_seconds * (1.0 + 1e-12);
                    for frequency in [10.0, 100.0, 1_000.0, 10_000.0, 0.5 * rate] {
                        let rt60 = damped.rt60_at(frequency);
                        assert!(rt60 <= previous, "{frequency} Hz: {rt60} after {previous}");
                        previous = rt60;
                    }
                }
            }
        }
        // The decay as a curve, and the energy of the tail against time.
        let decay = ReverbDecay::new(2.0, 0.5, 0.5, 48_000.0);
        let spec = decay.spec();
        assert_eq!((spec.kind, spec.y.max), (CurveKind::DecayTime, 2.4));
        assert_eq!(decay.value_at(3_000.0), decay.rt60_at(3_000.0));
        let energy = decay.energy_at(3_000.0);
        let rt60 = decay.rt60_at(3_000.0);
        assert!((energy.value_at(rt60) + 60.0).abs() < 1e-9);
        assert_eq!(energy.value_at(-1.0), 0.0);
        assert_eq!(energy.spec().kind, CurveKind::Decay);
        // Garbage gives numbers.
        let broken = ReverbDecay::new(f64::NAN, f64::NAN, f64::NAN, f64::NAN);
        assert!(broken.rt60_at(1_000.0).is_finite());
        assert!(broken.energy_at(1_000.0).value_at(1.0).is_finite());
    }
}
