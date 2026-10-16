//! Curves that show what a processor does, computed from its settings.
//!
//! An interface that draws an equaliser's response, a compressor's
//! transfer curve or an envelope's shape should not imitate the processor.
//! The curves here come from the formulas, limits and coefficients the
//! processor itself runs on, so what is drawn is what is heard.
//!
//! Each curve is described by a small `Copy` value that holds settings and
//! nothing else: no memory of the signal, no buffers. It can be made in two
//! ways, which give the same value:
//!
//! - from the processor, with a method such as
//!   [`Filter::response`](crate::Filter::response), which takes the
//!   settings the processor is at or heading for (where a glide arrives);
//! - from settings alone, with the descriptor's own constructor, on any
//!   thread that holds a copy of them, such as an interface that reads
//!   parameter values: [`SvfResponse::new`] applies the same limits as
//!   [`StateVariableFilter`](crate::StateVariableFilter) does.
//!
//! | Processor | Descriptor | What it draws |
//! |---|---|---|
//! | [`Filter`](crate::Filter), [`Biquad`](crate::Biquad) | [`BiquadResponse`] | frequency response |
//! | [`BiquadCascade`](crate::BiquadCascade), [`IirFilter`](crate::IirFilter), [`Design`](crate::iir::Design) | [`CascadeResponse`] | frequency response |
//! | [`StateVariableFilter`](crate::StateVariableFilter) | [`SvfResponse`] | frequency response |
//! | [`OnePole`](crate::OnePole) | [`OnePoleResponse`] | frequency response |
//! | [`LadderFilter`](crate::LadderFilter) | [`LadderResponse`] | frequency response of its linear model |
//! | [`Crossover`](crate::Crossover) | [`CrossoverResponse`] | each band, or their sum |
//! | [`FirFilter`](crate::FirFilter) | [`FirResponse`] | frequency response |
//! | several of the above in a row | a slice, an array or a tuple of them | the response of the chain |
//! | [`Compressor`](crate::Compressor) | [`CompressorTransfer`] | output level against input level |
//! | [`Expander`](crate::Expander) | [`ExpanderTransfer`] | output level against input level |
//! | [`NoiseGate`](crate::NoiseGate) | [`GateTransfer`] | output level against input level |
//! | [`Limiter`](crate::Limiter) | [`LimiterTransfer`] | output level against input level |
//! | [`Waveshaper`](crate::Waveshaper), [`ShaperCurve`](crate::ShaperCurve) | themselves | output value against input value |
//! | [`Adsr`](crate::Adsr), [`AdsrParams`](crate::AdsrParams) | [`EnvelopeShape`] | level against time |
//! | [`Lfo`](crate::Lfo) | [`LfoCycle`] | one cycle |
//! | [`Chorus`](crate::Chorus) (and its flanger) | [`ChorusSweep`] | the delay of a voice over one cycle |
//! | [`Phaser`](crate::Phaser) | [`PhaserSweep`] | the frequency of the stages over one cycle |
//! | [`Delay`](crate::Delay) | [`DelayTaps`] | the time and gain of each echo |
//! | [`Reverb`](crate::Reverb) | [`ReverbDecay`] | decay time against frequency, and level against time |
//!
//! Every continuous curve implements [`Curve`]: a [`CurveSpec`] that says
//! what the axes are (their [`Unit`], [`Scale`] and a suggested range) and
//! the value at any point of the horizontal axis. Frequency responses
//! implement [`FrequencyResponse`], which gives the complex response, and
//! become curves through [`magnitude`](FrequencyResponse::magnitude) and
//! [`phase`](FrequencyResponse::phase). A [`DelayTaps`] is a set of points
//! rather than a curve, drawn as stems.
//!
//! # Threads
//!
//! Nothing here allocates, locks or touches a processor's memory. A
//! descriptor can be made on the audio thread and sent through a queue,
//! or made on the interface thread from parameter values; computing a curve
//! is real-time safe, though it is meant for the interface. The values are
//! always finite numbers: a gain of zero, at the centre of a notch or below
//! a closed gate, is given as [`MIN_DB`], and an infinite one, exactly on a
//! pole at the edge of self-oscillation, as [`MAX_DB`].
//!
//! # Drawing a curve
//!
//! ```
//! use manguebyte_dsp::FilterShape;
//! use manguebyte_dsp::curve::{BiquadResponse, Curve, FrequencyResponse, Scale, Unit};
//!
//! // Two bands of an equaliser: their curve is the sum of their decibels.
//! let rate = 48_000.0;
//! let equaliser = [
//!     BiquadResponse::design(FilterShape::LowShelf { gain_db: 4.0 }, 120.0, 0.707, rate),
//!     BiquadResponse::design(FilterShape::Peak { gain_db: -6.0 }, 2_500.0, 1.4, rate),
//! ];
//! let curve = equaliser.magnitude();
//!
//! // Hertz on a logarithmic scale against decibels, with a range to fit.
//! let spec = curve.spec();
//! assert_eq!((spec.x.unit, spec.x.scale), (Unit::Hertz, Scale::Logarithmic));
//! assert_eq!((spec.y.min, spec.y.max), (-12.0, 12.0));
//!
//! // The frequencies to draw at, and the curve there.
//! let mut hertz = [0.0; 256];
//! spec.x.fill(&mut hertz);
//! let mut decibels = [0.0; 256];
//! curve.fill(&hertz, &mut decibels);
//! assert!((curve.value_at(2_500.0) + 6.0).abs() < 0.05);
//! assert!((decibels[0] - 4.0).abs() < 0.2);
//! ```

mod frequency;
mod time;
mod transfer;

pub use frequency::{
    BiquadResponse, CascadeResponse, CrossoverResponse, FirResponse, LadderResponse,
    OnePoleResponse, SvfResponse,
};
pub use time::{
    ChorusSweep, DelayTaps, EnvelopeShape, LfoCycle, PhaserSweep, ReverbDecay, ReverbEnergy,
};
pub use transfer::{CompressorTransfer, ExpanderTransfer, GateTransfer, LimiterTransfer};

use core::f64::consts::{PI, TAU};

use crate::fft::Complex;

/// The lowest level a curve gives, in decibels: what a gain of zero, the
/// centre of a notch or the output of a closed gate, is reported as, so that
/// every point can be drawn. It is a gain of `1e-20`, far below what any
/// sample format resolves.
pub const MIN_DB: f64 = -400.0;

/// The highest level a curve gives, in decibels: what an infinite gain, a
/// pole hit exactly, is reported as.
pub const MAX_DB: f64 = 400.0;

/// A gain in decibels, limited to [`MIN_DB`]`..=`[`MAX_DB`]. Zero, a
/// negative number and not-a-number give [`MIN_DB`].
pub fn gain_to_db(gain: f64) -> f64 {
    let db = 20.0 * gain.log10();
    if db >= MIN_DB { db.min(MAX_DB) } else { MIN_DB }
}

/// A point of a curve.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    /// Where it is along the horizontal axis.
    pub x: f64,
    /// Where it is along the vertical axis.
    pub y: f64,
}

impl Point {
    /// A point at `x`, `y`.
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// What the numbers along an axis measure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Unit {
    /// Frequency, in hertz.
    Hertz,
    /// A level or a gain, in decibels.
    Decibels,
    /// A phase, in radians.
    Radians,
    /// Time, in seconds.
    Seconds,
    /// Time, in milliseconds.
    Milliseconds,
    /// A plain number: a sample value, a gain, the level of an envelope or
    /// of a modulation.
    Value,
    /// A position in a cycle, from 0 at its start to 1 at its end.
    Cycles,
}

/// How the numbers are spread along an axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scale {
    /// Equal steps of the number take equal room.
    Linear,
    /// Equal ratios of the number take equal room, as octaves do along a
    /// frequency axis. Every number on it is above zero.
    Logarithmic,
}

/// One axis of a curve: what it measures, how, and the range to show.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Axis {
    /// What the numbers measure.
    pub unit: Unit,
    /// How they are spread.
    pub scale: Scale,
    /// The number at the start of the axis.
    pub min: f64,
    /// The number at the end of the axis.
    pub max: f64,
}

impl Axis {
    /// An axis of `unit` and `scale`, from `min` to `max`.
    pub const fn new(unit: Unit, scale: Scale, min: f64, max: f64) -> Self {
        Self {
            unit,
            scale,
            min,
            max,
        }
    }

    /// The audible frequencies a filter at `sample_rate` can pass: from
    /// 20 Hz to 20 kHz or half the sample rate, whichever is lower, on a
    /// logarithmic scale. A sample rate too low for that range starts the
    /// axis a decade below its end instead.
    pub fn frequency(sample_rate: f64) -> Self {
        let top = if sample_rate.is_finite() && sample_rate > 0.0 {
            (0.5 * sample_rate).min(20_000.0)
        } else {
            20_000.0
        };
        Self::new(Unit::Hertz, Scale::Logarithmic, 20f64.min(0.1 * top), top)
    }

    /// Fills `values` with numbers evenly spread along the axis from
    /// [`min`](Self::min) to [`max`](Self::max), both included: the places
    /// to draw a curve at. See [`linear_spaced`] and [`log_spaced`].
    pub fn fill(&self, values: &mut [f64]) {
        match self.scale {
            Scale::Linear => linear_spaced(self.min, self.max, values),
            Scale::Logarithmic => log_spaced(self.min, self.max, values),
        }
    }

    /// Where `value` lies along the axis: 0 at [`min`](Self::min), 1 at
    /// [`max`](Self::max), outside that range beyond them. What a plot
    /// multiplies by its width or height.
    pub fn position(&self, value: f64) -> f64 {
        let (low, high, value) = match self.scale {
            Scale::Linear => (self.min, self.max, value),
            Scale::Logarithmic => (self.min.ln(), self.max.ln(), value.ln()),
        };
        (value - low) / (high - low)
    }
}

/// Fills `values` with numbers from `low` to `high`, both included, in
/// equal steps. A single value is `low`.
pub fn linear_spaced(low: f64, high: f64, values: &mut [f64]) {
    let steps = values.len().saturating_sub(1).max(1) as f64;
    for (index, value) in values.iter_mut().enumerate() {
        *value = low + (high - low) * (index as f64 / steps);
    }
    if values.len() > 1 {
        values[values.len() - 1] = high;
    }
}

/// Fills `values` with numbers from `low` to `high`, both included, in
/// equal ratios: a frequency grid with as many points in every octave. A
/// single value is `low`. Both ends must be above zero; one that is not
/// (zero, a negative number, not-a-number) is taken as the smallest
/// positive number, and an infinite one as the largest.
pub fn log_spaced(low: f64, high: f64, values: &mut [f64]) {
    let positive = |x: f64| {
        if x > 0.0 {
            x.min(f64::MAX)
        } else {
            f64::MIN_POSITIVE
        }
    };
    let (low, high) = (positive(low), positive(high));
    let (start, span) = (low.ln(), (high / low).ln());
    let steps = values.len().saturating_sub(1).max(1) as f64;
    for (index, value) in values.iter_mut().enumerate() {
        *value = (start + span * (index as f64 / steps)).exp();
    }
    if let Some(first) = values.first_mut() {
        *first = low;
    }
    if values.len() > 1 {
        values[values.len() - 1] = high;
    }
}

/// Removes the jumps of a whole turn from a run of phases, such as
/// [`Phase`] fills along a frequency grid: wherever one phase is more than
/// half a turn from the one before, whole turns are added or taken away
/// until it is not. The result is a continuous curve that can go beyond
/// `-π..=π`.
pub fn unwrap_phase(phases: &mut [f64]) {
    let mut offset = 0.0;
    let mut previous: Option<f64> = None;
    for phase in phases {
        let raw = *phase;
        if let Some(previous) = previous {
            let step = raw + offset - previous;
            if step.is_finite() {
                offset -= TAU * (step / TAU).round();
            }
        }
        *phase = raw + offset;
        previous = Some(*phase);
    }
}

/// What a curve shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CurveKind {
    /// The gain of a filter against frequency: hertz on a logarithmic scale
    /// against decibels.
    Magnitude,
    /// The phase of a filter against frequency: hertz on a logarithmic
    /// scale against radians.
    Phase,
    /// The level that comes out against the level that goes in, of a steady
    /// signal: decibels against decibels.
    LevelTransfer,
    /// The value that comes out against the value that goes in, sample by
    /// sample: a plain number against a plain number.
    ValueTransfer,
    /// The level of an envelope against time: seconds against a plain
    /// number from 0 to 1.
    Envelope,
    /// A modulation over one cycle: cycles against what it moves (a plain
    /// number, milliseconds of delay, hertz).
    Modulation,
    /// Separate events against time, drawn as stems: seconds against a
    /// gain.
    Taps,
    /// How a sound dies away: seconds against decibels.
    Decay,
    /// How long a sound takes to die away against frequency: hertz on a
    /// logarithmic scale against seconds.
    DecayTime,
}

/// What a curve shows and how to draw its axes.
///
/// The ranges are suggestions fitted to the curve: an interface may use
/// its own, and a curve can leave the range it suggests (a notch is deeper
/// than any range shows).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CurveSpec {
    /// What the curve shows.
    pub kind: CurveKind,
    /// The horizontal axis: what the curve is a function of.
    pub x: Axis,
    /// The vertical axis.
    pub y: Axis,
}

/// A curve: a value for every point of the horizontal axis.
///
/// Implemented by every continuous descriptor of this module, and by
/// [`Magnitude`] and [`Phase`] for frequency responses. It is object safe,
/// so an interface can draw whatever an effect hands it as a `&dyn Curve`.
pub trait Curve {
    /// What the curve shows, and the axes to draw it on.
    fn spec(&self) -> CurveSpec;

    /// The value of the curve at `x`, in the unit of the vertical axis.
    /// Always a finite number.
    fn value_at(&self, x: f64) -> f64;

    /// Writes the value at each of `xs` to `ys`, over as many as both have.
    fn fill(&self, xs: &[f64], ys: &mut [f64]) {
        for (y, &x) in ys.iter_mut().zip(xs) {
            *y = self.value_at(x);
        }
    }
}

impl<C: Curve + ?Sized> Curve for &C {
    fn spec(&self) -> CurveSpec {
        (**self).spec()
    }
    fn value_at(&self, x: f64) -> f64 {
        (**self).value_at(x)
    }
    fn fill(&self, xs: &[f64], ys: &mut [f64]) {
        (**self).fill(xs, ys);
    }
}

/// The response of a linear filter to a steady sine, at any frequency.
///
/// [`response_at`](Self::response_at) is the complex gain: its size is how
/// much the filter scales a sine of that frequency, its angle how far it
/// turns the sine's phase (negative is a delay). Each implementation works
/// it out from the coefficients the filter runs on, exactly, not from a
/// measurement.
///
/// A chain of filters is a filter too: a slice, an array or a tuple (of up
/// to eight members) of responses is the response of its members one after
/// another, whose gain in decibels is the sum of theirs and whose phase is
/// the sum of their phases. The members are taken to be at the same sample
/// rate, that of the first.
pub trait FrequencyResponse {
    /// The sample rate the filter runs at, in hertz.
    fn sample_rate(&self) -> f64;

    /// The complex gain at `frequency` hertz, which is limited to between 0
    /// and half the sample rate.
    fn response_at(&self, frequency: f64) -> Complex<f64>;

    /// The gain at `frequency` hertz, in decibels, limited to
    /// [`MIN_DB`]`..=`[`MAX_DB`].
    fn magnitude_db_at(&self, frequency: f64) -> f64 {
        let response = self.response_at(frequency);
        gain_to_db(response.re.hypot(response.im))
    }

    /// The phase shift at `frequency` hertz, in radians, in `-π..=π`.
    /// Negative is a delay. Where the gain is zero the phase has no
    /// meaning, and this gives 0 or the limit approached.
    fn phase_at(&self, frequency: f64) -> f64 {
        phase_of(self.response_at(frequency))
    }

    /// The gain as a [`Curve`]: hertz against decibels. For a `&dyn
    /// FrequencyResponse`, which cannot call this, make the curve as
    /// `Magnitude(response)`.
    fn magnitude(&self) -> Magnitude<&Self>
    where
        Self: Sized,
    {
        Magnitude(self)
    }

    /// The phase as a [`Curve`]: hertz against radians. For a `&dyn
    /// FrequencyResponse`, make the curve as `Phase(response)`.
    fn phase(&self) -> Phase<&Self>
    where
        Self: Sized,
    {
        Phase(self)
    }
}

/// The angle of `response`, or 0 for something that is not a finite
/// number.
fn phase_of(response: Complex<f64>) -> f64 {
    let angle = response.im.atan2(response.re);
    if angle.is_finite() { angle } else { 0.0 }
}

/// An angle in `-π..=π`.
fn wrap(angle: f64) -> f64 {
    if !angle.is_finite() {
        return 0.0;
    }
    let wrapped = angle - TAU * (angle / TAU).round();
    if wrapped <= -PI {
        wrapped + TAU
    } else {
        wrapped
    }
}

/// The sample rate a chain without members reports: the one every
/// processor assumes until it is prepared.
const UNPREPARED_RATE: f64 = 44_100.0;

impl<R: FrequencyResponse + ?Sized> FrequencyResponse for &R {
    fn sample_rate(&self) -> f64 {
        (**self).sample_rate()
    }
    fn response_at(&self, frequency: f64) -> Complex<f64> {
        (**self).response_at(frequency)
    }
    fn magnitude_db_at(&self, frequency: f64) -> f64 {
        (**self).magnitude_db_at(frequency)
    }
    fn phase_at(&self, frequency: f64) -> f64 {
        (**self).phase_at(frequency)
    }
}

impl<R: FrequencyResponse> FrequencyResponse for [R] {
    fn sample_rate(&self) -> f64 {
        self.first().map_or(UNPREPARED_RATE, R::sample_rate)
    }
    fn response_at(&self, frequency: f64) -> Complex<f64> {
        self.iter().fold(Complex::new(1.0, 0.0), |product, member| {
            product * member.response_at(frequency)
        })
    }
    fn magnitude_db_at(&self, frequency: f64) -> f64 {
        let db: f64 = self
            .iter()
            .map(|member| member.magnitude_db_at(frequency))
            .sum();
        db.clamp(MIN_DB, MAX_DB)
    }
    fn phase_at(&self, frequency: f64) -> f64 {
        wrap(self.iter().map(|member| member.phase_at(frequency)).sum())
    }
}

impl<R: FrequencyResponse, const N: usize> FrequencyResponse for [R; N] {
    fn sample_rate(&self) -> f64 {
        self.as_slice().sample_rate()
    }
    fn response_at(&self, frequency: f64) -> Complex<f64> {
        self.as_slice().response_at(frequency)
    }
    fn magnitude_db_at(&self, frequency: f64) -> f64 {
        self.as_slice().magnitude_db_at(frequency)
    }
    fn phase_at(&self, frequency: f64) -> f64 {
        self.as_slice().phase_at(frequency)
    }
}

macro_rules! impl_response_for_tuple {
    ($first:ident $(, $member:ident)*) => {
        #[allow(non_snake_case)]
        impl<$first: FrequencyResponse $(, $member: FrequencyResponse)*> FrequencyResponse
            for ($first, $($member,)*)
        {
            fn sample_rate(&self) -> f64 {
                self.0.sample_rate()
            }
            fn response_at(&self, frequency: f64) -> Complex<f64> {
                let ($first, $($member,)*) = self;
                $first.response_at(frequency) $(* $member.response_at(frequency))*
            }
            fn magnitude_db_at(&self, frequency: f64) -> f64 {
                let ($first, $($member,)*) = self;
                let db = $first.magnitude_db_at(frequency)
                    $(+ $member.magnitude_db_at(frequency))*;
                db.clamp(MIN_DB, MAX_DB)
            }
            fn phase_at(&self, frequency: f64) -> f64 {
                let ($first, $($member,)*) = self;
                wrap($first.phase_at(frequency) $(+ $member.phase_at(frequency))*)
            }
        }
    };
}

impl_response_for_tuple!(A);
impl_response_for_tuple!(A, B);
impl_response_for_tuple!(A, B, C);
impl_response_for_tuple!(A, B, C, D);
impl_response_for_tuple!(A, B, C, D, E);
impl_response_for_tuple!(A, B, C, D, E, F);
impl_response_for_tuple!(A, B, C, D, E, F, G);
impl_response_for_tuple!(A, B, C, D, E, F, G, H);

/// The gain of a [`FrequencyResponse`] as a [`Curve`]: hertz on a
/// logarithmic scale against decibels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Magnitude<R>(pub R);

/// How many frequencies [`Magnitude::spec`] looks at to fit its range.
const FIT_POINTS: usize = 64;

impl<R: FrequencyResponse> Curve for Magnitude<R> {
    /// The frequencies of [`Axis::frequency`] against decibels, in a range
    /// fitted to the curve over them: from 6 dB above its highest point,
    /// in steps of 6 dB, to as far below 0 dB, unless the curve goes lower,
    /// in which case to 6 dB below its lowest point, but no lower than
    /// -120 dB. An equaliser that boosts by 4 dB and cuts by 6 is shown
    /// from -12 to +12 dB; a low-pass filter from 6 dB down to as far as
    /// its stop band, or -120 dB.
    fn spec(&self) -> CurveSpec {
        let x = Axis::frequency(self.0.sample_rate());
        let mut hertz = [0.0; FIT_POINTS];
        x.fill(&mut hertz);
        let (mut lowest, mut highest) = (0.0f64, 0.0f64);
        for frequency in hertz {
            let db = self.0.magnitude_db_at(frequency);
            lowest = lowest.min(db);
            highest = highest.max(db);
        }
        let top = (6.0 * ((highest + 3.0) / 6.0).ceil()).clamp(6.0, MAX_DB);
        let bottom = if lowest >= -top {
            -top
        } else {
            (6.0 * ((lowest - 3.0) / 6.0).floor()).max(-120.0)
        };
        CurveSpec {
            kind: CurveKind::Magnitude,
            x,
            y: Axis::new(Unit::Decibels, Scale::Linear, bottom, top),
        }
    }

    fn value_at(&self, frequency: f64) -> f64 {
        self.0.magnitude_db_at(frequency)
    }
}

/// The phase of a [`FrequencyResponse`] as a [`Curve`]: hertz on a
/// logarithmic scale against radians, from -π to π. Pass what
/// [`fill`](Curve::fill) writes through [`unwrap_phase`] to draw it without
/// jumps.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Phase<R>(pub R);

impl<R: FrequencyResponse> Curve for Phase<R> {
    fn spec(&self) -> CurveSpec {
        CurveSpec {
            kind: CurveKind::Phase,
            x: Axis::frequency(self.0.sample_rate()),
            y: Axis::new(Unit::Radians, Scale::Linear, -PI, PI),
        }
    }

    fn value_at(&self, frequency: f64) -> f64 {
        self.0.phase_at(frequency)
    }
}

/// The level axis of a transfer curve that is worth showing from
/// `lowest_db` up: from 12 dB below that, in steps of 6 dB, but no higher
/// than -60 dB and no lower than -120 dB, up to +6 dB.
fn level_axis(lowest_db: f64) -> Axis {
    let low = if lowest_db.is_finite() {
        (6.0 * ((lowest_db - 12.0) / 6.0).floor()).clamp(-120.0, -60.0)
    } else {
        -60.0
    };
    Axis::new(Unit::Decibels, Scale::Linear, low, 6.0)
}

/// The spec of a transfer curve whose interesting part starts at
/// `lowest_db` and whose output at the top of the input axis is `top_db`:
/// the same range on both axes, raised at the top of the output axis to
/// show `top_db`.
fn level_transfer_spec(lowest_db: f64, top_db: f64) -> CurveSpec {
    let x = level_axis(lowest_db);
    let mut y = x;
    if top_db > y.max {
        y.max = (6.0 * (top_db / 6.0).ceil()).min(MAX_DB);
    }
    CurveSpec {
        kind: CurveKind::LevelTransfer,
        x,
        y,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decibels_are_always_finite() {
        assert_eq!(gain_to_db(1.0), 0.0);
        assert!((gain_to_db(0.5) + 6.020_599_913).abs() < 1e-9);
        assert_eq!(gain_to_db(0.0), MIN_DB);
        assert_eq!(gain_to_db(-1.0), MIN_DB);
        assert_eq!(gain_to_db(f64::NAN), MIN_DB);
        assert_eq!(gain_to_db(f64::INFINITY), MAX_DB);
        assert_eq!(gain_to_db(1e-30), MIN_DB);
    }

    #[test]
    fn grids_include_both_ends_and_are_evenly_spread() {
        let mut values = [0.0; 7];
        log_spaced(20.0, 20_480.0, &mut values);
        assert_eq!(values[0], 20.0);
        assert_eq!(values[6], 20_480.0);
        // Ten octaves in six steps: every step is the same ratio.
        let ratio = 2f64.powf(10.0 / 6.0);
        for pair in values.windows(2) {
            assert!((pair[1] / pair[0] - ratio).abs() < 1e-12);
        }
        linear_spaced(-1.0, 1.0, &mut values);
        for (n, value) in values.iter().enumerate() {
            assert!((value - (n as f64 / 3.0 - 1.0)).abs() < 1e-15);
        }
        assert_eq!((values[0], values[3], values[6]), (-1.0, 0.0, 1.0));

        let mut one = [0.0];
        log_spaced(5.0, 50.0, &mut one);
        assert_eq!(one, [5.0]);
        linear_spaced(5.0, 50.0, &mut one);
        assert_eq!(one, [5.0]);
        log_spaced(5.0, 50.0, &mut []);

        // Garbage gives numbers, not a panic.
        log_spaced(0.0, f64::NAN, &mut values);
        assert!(values.iter().all(|v| v.is_finite() && *v > 0.0));
        log_spaced(-3.0, f64::INFINITY, &mut values);
        assert!(values.iter().all(|v| *v > 0.0));
    }

    #[test]
    fn an_axis_places_values_along_its_scale() {
        let axis = Axis::frequency(48_000.0);
        assert_eq!((axis.min, axis.max), (20.0, 20_000.0));
        assert_eq!(axis.position(20.0), 0.0);
        assert!((axis.position(20_000.0) - 1.0).abs() < 1e-12);
        // 632 Hz is half-way between 20 Hz and 20 kHz in octaves.
        assert!((axis.position(20.0 * 1_000f64.sqrt()) - 0.5).abs() < 1e-12);
        assert_eq!(Axis::frequency(22_050.0).max, 11_025.0);
        assert_eq!(Axis::frequency(100.0).min, 5.0);
        assert_eq!(Axis::frequency(f64::NAN).max, 20_000.0);

        let level = Axis::new(Unit::Decibels, Scale::Linear, -60.0, 0.0);
        assert_eq!(level.position(-30.0), 0.5);
        let mut values = [0.0; 3];
        level.fill(&mut values);
        assert_eq!(values, [-60.0, -30.0, 0.0]);
    }

    #[test]
    fn unwrapping_removes_whole_turns() {
        // A delay of 10 samples, sampled every 0.05 of the sample rate:
        // a phase falling by π each step, wrapped.
        let mut phases: Vec<f64> = (0..20).map(|n| wrap(-PI * 0.9 * n as f64)).collect();
        unwrap_phase(&mut phases);
        for (n, phase) in phases.iter().enumerate() {
            assert!((phase + PI * 0.9 * n as f64).abs() < 1e-9, "{n}: {phase}");
        }
        let mut odd = [0.0, f64::NAN, 1.0];
        unwrap_phase(&mut odd);
        assert_eq!(odd[0], 0.0);
        unwrap_phase(&mut []);
    }

    #[test]
    fn angles_wrap_into_a_half_turn_either_side() {
        assert_eq!(wrap(0.0), 0.0);
        assert!((wrap(3.0 * PI) - PI).abs() < 1e-12);
        assert!((wrap(-3.0 * PI) - PI).abs() < 1e-12);
        assert!((wrap(TAU + 0.5) - 0.5).abs() < 1e-12);
        assert_eq!(wrap(f64::NAN), 0.0);
        assert_eq!(phase_of(Complex::new(f64::NAN, 1.0)), 0.0);
    }

    #[test]
    fn transfer_axes_fit_the_interesting_part() {
        assert_eq!(level_axis(-18.0).min, -60.0);
        assert_eq!(level_axis(-70.0).min, -84.0);
        assert_eq!(level_axis(-500.0).min, -120.0);
        assert_eq!(level_axis(f64::NAN).min, -60.0);
        let spec = level_transfer_spec(-18.0, 10.0);
        assert_eq!((spec.x.max, spec.y.max), (6.0, 12.0));
    }
}
