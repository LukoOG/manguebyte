//! Frequency responses of the filters, from the coefficients they run on.

use core::f64::consts::{PI, SQRT_2, TAU};

use manguebyte_core::Sample;

use super::FrequencyResponse;
use crate::cascade::Row;
use crate::fft::Complex;
use crate::numeric::limit;
use crate::{
    BiquadCoefficients, CrossoverSlope, FilterShape, LadderMode, OnePoleMode, SvfMode, crossover,
    iir, ladder, onepole, svf,
};

type C = Complex<f64>;

const ONE: C = C::new(1.0, 0.0);

/// `a / b`, by R. L. Smith's method, which neither overflows nor
/// underflows where the quotient itself can be represented ("Algorithm
/// 116: Complex division", Communications of the ACM 5(8), 1962). A
/// division by zero, which only a pole hit exactly can cause, gives an
/// infinite real number.
fn divide(a: C, b: C) -> C {
    if b.re == 0.0 && b.im == 0.0 {
        return C::new(f64::INFINITY, 0.0);
    }
    if b.re.abs() >= b.im.abs() {
        let ratio = b.im / b.re;
        let scale = b.re + b.im * ratio;
        C::new((a.re + a.im * ratio) / scale, (a.im - a.re * ratio) / scale)
    } else {
        let ratio = b.re / b.im;
        let scale = b.re * ratio + b.im;
        C::new((a.re * ratio + a.im) / scale, (a.im * ratio - a.re) / scale)
    }
}

/// `tan(π·f/fs)`, the frequency `f` as the bilinear transform maps it onto
/// the analogue frequency axis: infinite at half the sample rate. `f` is
/// limited to between 0 and half the sample rate.
fn prewarped(frequency: f64, sample_rate: f64) -> f64 {
    let frequency = limit(frequency, 0.0, 0.5 * sample_rate);
    (PI * frequency / sample_rate).tan()
}

/// `(high·σ² + band·σ + low) / (σ² + k·σ + 1)` at `σ = j·w`, for `w` of
/// zero or more.
///
/// This is a second-order section in state-variable form (see
/// [`BiquadCascade`](crate::BiquadCascade)), and also the state-variable
/// filter with its outputs weighted. Above `w = 1` the numerator and the
/// denominator are both divided by `w²`, so that half the sample rate,
/// where `w` is infinite, gives the limit. `1 - w²` is taken as
/// `(1 - w)·(1 + w)`, which keeps its digits where it is small: at the
/// cutoff, where a resonance is decided.
fn second_order(high: f64, band: f64, low: f64, k: f64, w: f64) -> C {
    if w <= 1.0 {
        let numerator = C::new(low - high * w * w, band * w);
        let denominator = C::new((1.0 - w) * (1.0 + w), k * w);
        divide(numerator, denominator)
    } else {
        let v = 1.0 / w;
        let numerator = C::new(low * v * v - high, band * v);
        let denominator = C::new((v - 1.0) * (v + 1.0), k * v);
        divide(numerator, denominator)
    }
}

/// `e^(jω)·(c0 + c1·e^(-jω) + c2·e^(-2jω))`, which is
/// `(c0 + c2)·cos ω + c1 + j·(c0 - c2)·sin ω`.
///
/// The real part is expanded around whichever of 0 Hz and half the sample
/// rate is nearer, as in [`Sections::magnitude_at`](iir::Sections::magnitude_at),
/// so that a pole or a zero close to either keeps its digits. The factor
/// `e^(jω)` is the same for a numerator and a denominator, so their
/// quotient is the response, phase and all.
fn rotated(c0: f64, c1: f64, c2: f64, omega: f64) -> C {
    let outer = c0 + c2;
    let real = if omega < PI / 2.0 {
        let half = (omega / 2.0).sin();
        ((c0 + c1) + c2) - outer * 2.0 * half * half
    } else {
        let half = (omega / 2.0).cos();
        ((c1 - c0) - c2) + outer * 2.0 * half * half
    };
    C::new(real, (c0 - c2) * omega.sin())
}

/// The frequency response of one second-order section in direct form: a
/// [`Biquad`](crate::Biquad) or a [`Filter`](crate::Filter).
///
/// ```
/// use manguebyte_dsp::FilterShape;
/// use manguebyte_dsp::curve::{BiquadResponse, FrequencyResponse};
///
/// let peak = BiquadResponse::design(FilterShape::Peak { gain_db: 6.0 }, 1_000.0, 2.0, 48_000.0);
/// assert!((peak.magnitude_db_at(1_000.0) - 6.0).abs() < 1e-9);
/// // A peak turns the phase either side of its centre, not at it.
/// assert!(peak.phase_at(1_000.0).abs() < 1e-9);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BiquadResponse {
    coefficients: BiquadCoefficients,
    sample_rate: f64,
}

impl BiquadResponse {
    /// The response of `coefficients`, run at `sample_rate`.
    pub fn new(coefficients: BiquadCoefficients, sample_rate: f64) -> Self {
        Self {
            coefficients,
            sample_rate,
        }
    }

    /// The response of the filter [`BiquadCoefficients::design`] makes:
    /// what a [`Filter<f64>`](crate::Filter) with these settings runs.
    /// For a `Filter<f32>`, which rounds its coefficients to single
    /// precision, use [`rounded_to`](Self::rounded_to) too.
    pub fn design(shape: FilterShape, frequency: f64, q: f64, sample_rate: f64) -> Self {
        Self::new(
            BiquadCoefficients::design(shape, sample_rate, frequency, q),
            sample_rate,
        )
    }

    /// The same response with the coefficients rounded to the precision
    /// of `S`, as a [`Biquad<S>`](crate::Biquad) holds them. In single
    /// precision that moves the response of a filter tuned low, below its
    /// cutoff: that of a second-order low-pass at 20 Hz by 0.02 dB at
    /// 48 kHz, 0.2 dB at 10 Hz and 48 kHz, and 0.9 dB at 20 Hz and
    /// 192 kHz.
    #[must_use]
    pub fn rounded_to<S: Sample>(self) -> Self {
        Self::new(self.coefficients.rounded_to::<S>(), self.sample_rate)
    }

    /// The coefficients.
    pub fn coefficients(&self) -> BiquadCoefficients {
        self.coefficients
    }
}

impl FrequencyResponse for BiquadResponse {
    fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    fn response_at(&self, frequency: f64) -> C {
        let frequency = limit(frequency, 0.0, 0.5 * self.sample_rate);
        let omega = TAU * frequency / self.sample_rate;
        let c = &self.coefficients;
        divide(
            rotated(c.b0, c.b1, c.b2, omega),
            rotated(1.0, c.a1, c.a2, omega),
        )
    }
}

/// The frequency response of second-order sections run one after another
/// by a [`BiquadCascade`](crate::BiquadCascade) or an
/// [`IirFilter`](crate::IirFilter), or designed by an
/// [`iir::Design`].
///
/// It is worked out from the form the cascade runs its sections in, the
/// state-variable form, rather than from their [`BiquadCoefficients`], so
/// it is the response of the cascade to the last digit even for the designs
/// whose coefficients cannot hold their poles (see [the design
/// module](crate::iir#precision)). With `w = tan(π·f/fs) / g`, a section is
/// `(low - high·w² + j·band·w) / (1 - w² + j·k·w)`.
///
/// ```
/// use manguebyte_dsp::curve::FrequencyResponse;
/// use manguebyte_dsp::iir::{Band, Design};
///
/// // An eighth-order Butterworth low-pass: 3.01 dB down at its cutoff and
/// // about 48 dB down an octave above it.
/// let response = Design::butterworth(Band::Lowpass { cutoff_hz: 1_000.0 }, 8).response(48_000.0);
/// assert!((response.magnitude_db_at(1_000.0) + 3.0103).abs() < 1e-4);
/// assert!((response.magnitude_db_at(2_000.0) + 48.0).abs() < 1.0);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CascadeResponse {
    rows: [Row; iir::MAX_SECTIONS],
    len: usize,
    sample_rate: f64,
}

impl CascadeResponse {
    /// The response of sections in state-variable form; those beyond
    /// [`iir::MAX_SECTIONS`] are left out, as a cascade leaves them out.
    pub(crate) fn from_rows(rows: &[Row], sample_rate: f64) -> Self {
        let len = rows.len().min(iir::MAX_SECTIONS);
        let mut all = [crate::cascade::IDENTITY; iir::MAX_SECTIONS];
        all[..len].copy_from_slice(&rows[..len]);
        Self {
            rows: all,
            len,
            sample_rate,
        }
    }

    /// How many sections there are.
    pub fn num_sections(&self) -> usize {
        self.len
    }
}

impl FrequencyResponse for CascadeResponse {
    fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    fn response_at(&self, frequency: f64) -> C {
        let t = prewarped(frequency, self.sample_rate);
        self.rows[..self.len]
            .iter()
            .fold(ONE, |product, &[g, k, high, band, low]| {
                product * second_order(high, band, low, k, t / g)
            })
    }
}

/// The frequency response of a
/// [`StateVariableFilter`](crate::StateVariableFilter).
///
/// The filter is the bilinear transform, with its cutoff pre-warped, of the
/// analogue `1 / (σ² + k·σ + 1)` and its relatives, so with `σ = j·tan(π·f/fs)
/// / g` the response is, for each mode: low-pass `1`, high-pass `σ²`,
/// band-pass `k·σ`, notch `σ² + 1` and all-pass `σ² - k·σ + 1`, over `σ² +
/// k·σ + 1`.
///
/// ```
/// use manguebyte_dsp::SvfMode;
/// use manguebyte_dsp::curve::{FrequencyResponse, SvfResponse};
///
/// // A resonant low-pass peaks by its Q (here 4, 12 dB) at its cutoff.
/// let response = SvfResponse::new(SvfMode::Lowpass, 1_000.0, 4.0, 48_000.0);
/// assert!((response.magnitude_db_at(1_000.0) - 12.041).abs() < 1e-3);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SvfResponse {
    mode: SvfMode,
    g: f64,
    k: f64,
    sample_rate: f64,
}

impl SvfResponse {
    /// The response of a filter of `mode` at `frequency` with resonance
    /// `q`, run at `sample_rate`, with the limits the filter applies: the
    /// cutoff to between 1 Hz and 0.49 of the sample rate, `q` to 0.01 or
    /// more.
    pub fn new(mode: SvfMode, frequency: f64, q: f64, sample_rate: f64) -> Self {
        let (g, k) = svf::tuning(frequency, q, sample_rate);
        Self::from_coefficients(mode, g, k, sample_rate)
    }

    /// The response of a filter running on the integrator gain `g` and the
    /// damping `k` of [`StateVariableFilter::coefficients`](crate::StateVariableFilter::coefficients).
    pub(crate) fn from_coefficients(mode: SvfMode, g: f64, k: f64, sample_rate: f64) -> Self {
        Self {
            mode,
            g,
            k,
            sample_rate,
        }
    }

    /// The mode.
    pub fn mode(&self) -> SvfMode {
        self.mode
    }
}

impl FrequencyResponse for SvfResponse {
    fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    fn response_at(&self, frequency: f64) -> C {
        let w = prewarped(frequency, self.sample_rate) / self.g;
        let k = self.k;
        let (high, band, low) = match self.mode {
            SvfMode::Lowpass => (0.0, 0.0, 1.0),
            SvfMode::Highpass => (1.0, 0.0, 0.0),
            SvfMode::Bandpass => (0.0, k, 0.0),
            SvfMode::Notch => (1.0, 0.0, 1.0),
            SvfMode::Allpass => (1.0, -k, 1.0),
        };
        second_order(high, band, low, k, w)
    }
}

/// The frequency response of a [`OnePole`](crate::OnePole).
///
/// With `σ = j·tan(π·f/fs) / g`: low-pass `1 / (1 + σ)`, high-pass
/// `σ / (1 + σ)`, all-pass `(1 - σ) / (1 + σ)`.
///
/// ```
/// use manguebyte_dsp::OnePoleMode;
/// use manguebyte_dsp::curve::{FrequencyResponse, OnePoleResponse};
///
/// let response = OnePoleResponse::new(OnePoleMode::Lowpass, 1_000.0, 48_000.0);
/// assert!((response.magnitude_db_at(1_000.0) + 3.0103).abs() < 1e-4);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OnePoleResponse {
    mode: OnePoleMode,
    g: f64,
    sample_rate: f64,
}

impl OnePoleResponse {
    /// The response of a filter of `mode` at `frequency`, run at
    /// `sample_rate`, with the limits the filter applies: the cutoff to
    /// between 0.01 Hz and 0.49 of the sample rate.
    pub fn new(mode: OnePoleMode, frequency: f64, sample_rate: f64) -> Self {
        Self::from_gain(
            mode,
            onepole::integrator_gain(frequency, sample_rate),
            sample_rate,
        )
    }

    /// The response of a filter whose integrator gain is `g`.
    pub(crate) fn from_gain(mode: OnePoleMode, g: f64, sample_rate: f64) -> Self {
        Self {
            mode,
            g,
            sample_rate,
        }
    }

    /// The mode.
    pub fn mode(&self) -> OnePoleMode {
        self.mode
    }
}

impl FrequencyResponse for OnePoleResponse {
    fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    fn response_at(&self, frequency: f64) -> C {
        let w = prewarped(frequency, self.sample_rate) / self.g;
        let (low, high) = self.mode.weights();
        divide(C::new(low, high * w), C::new(1.0, w))
    }
}

/// The frequency response of a [`LadderFilter`](crate::LadderFilter) for
/// signals well below the level of its saturator: its linear model.
///
/// With `σ = j·tan(π·f/fs) / g`, `L = 1 / (1 + σ)` the response of one
/// stage, `H = σ·L` that of a one-pole high-pass and `k = 4·resonance`, the
/// response is the drive times
///
/// ```text
/// Lp12: L²      Lp24: L⁴      Hp12: H²      Hp24: H⁴      Bp12: 2·L·H      Bp24: 4·L²·H²
/// ```
///
/// over `1 + k·L⁴`, the formulas in the filter's documentation. A louder
/// signal is held back by the saturator, which this does not show: at 0 dB
/// of drive a signal at full scale is already compressed by its peaks.
///
/// ```
/// use manguebyte_dsp::LadderMode;
/// use manguebyte_dsp::curve::{FrequencyResponse, LadderResponse};
///
/// // Resonance takes level away below the cutoff: 1 / (1 + 4 × 0.5).
/// let response = LadderResponse::new(LadderMode::Lp24, 1_000.0, 0.5, 0.0, 48_000.0);
/// let at_dc = response.magnitude_db_at(0.0);
/// assert!((at_dc - 20.0 * (1.0f64 / 3.0).log10()).abs() < 1e-9);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LadderResponse {
    mode: LadderMode,
    g: f64,
    feedback: f64,
    drive: f64,
    sample_rate: f64,
}

impl LadderResponse {
    /// The response of a filter of `mode` at `frequency`, with `resonance`
    /// and `drive_db`, run at `sample_rate`, with the limits the filter
    /// applies: the cutoff to between 5 Hz and 0.45 of the sample rate, the
    /// resonance to `0.0..=1.0`, the drive to `-24.0..=48.0` dB.
    pub fn new(
        mode: LadderMode,
        frequency: f64,
        resonance: f64,
        drive_db: f64,
        sample_rate: f64,
    ) -> Self {
        Self::from_parts(
            mode,
            ladder::integrator_gain(frequency, sample_rate),
            ladder::feedback(limit(resonance, 0.0, 1.0)),
            ladder::drive_gain(ladder::limited_drive_db(drive_db)),
            sample_rate,
        )
    }

    /// The response of a filter whose integrator gain is `g`, with
    /// `feedback` around the four stages and the plain gain `drive` in
    /// front of them.
    pub(crate) fn from_parts(
        mode: LadderMode,
        g: f64,
        feedback: f64,
        drive: f64,
        sample_rate: f64,
    ) -> Self {
        Self {
            mode,
            g,
            feedback,
            drive,
            sample_rate,
        }
    }

    /// The mode.
    pub fn mode(&self) -> LadderMode {
        self.mode
    }
}

impl FrequencyResponse for LadderResponse {
    fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    fn response_at(&self, frequency: f64) -> C {
        let w = prewarped(frequency, self.sample_rate) / self.g;
        let low = divide(ONE, C::new(1.0, w));
        // σ·L, written out so that half the sample rate gives 1 and not
        // infinity times zero.
        let high = divide(C::new(0.0, w), C::new(1.0, w));
        let low2 = low * low;
        let high2 = high * high;
        let numerator = match self.mode {
            LadderMode::Lp12 => low2,
            LadderMode::Lp24 => low2 * low2,
            LadderMode::Hp12 => high2,
            LadderMode::Hp24 => high2 * high2,
            LadderMode::Bp12 => (low * high).scale(2.0),
            LadderMode::Bp24 => (low2 * high2).scale(4.0),
        };
        let feedback = ONE + (low2 * low2).scale(self.feedback);
        divide(numerator.scale(self.drive), feedback)
    }
}

/// The frequency response of a band of a [`Crossover`](crate::Crossover),
/// or of the sum of its bands.
///
/// Each split at a crossover frequency, with `σ = j·tan(π·f/fs) / g` and
/// `B` the Butterworth polynomial the slope squares, has a low band `1 /
/// B(σ)²`, a high band `(-σ²)^N / B(σ)²` and an all-pass `B(-σ) / B(σ)`.
/// Band `b` of a crossover is the high band of every split below it, the
/// low band of its own split, and the all-pass of every split above that,
/// which the crossover runs it through so that the bands stay in phase.
/// The sum of the bands is the product of the all-passes: flat, with its
/// phase turned.
///
/// ```
/// use manguebyte_dsp::CrossoverSlope;
/// use manguebyte_dsp::curve::{CrossoverResponse, FrequencyResponse};
///
/// let crossover = CrossoverResponse::new(CrossoverSlope::Lr4, &[200.0, 2_000.0], 48_000.0);
/// let low = crossover.band(0).unwrap();
/// // Every band is 6.02 dB down at its crossover frequency, and the bands
/// // add up flat.
/// assert!((low.magnitude_db_at(200.0) + 6.0206).abs() < 1e-3);
/// assert!(crossover.magnitude_db_at(700.0).abs() < 1e-9);
/// assert!(crossover.band(3).is_none());
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CrossoverResponse {
    slope: CrossoverSlope,
    splits: usize,
    gains: [f64; crossover::MAX_SPLITS],
    /// The band, or `None` for the sum of the bands.
    band: Option<usize>,
    sample_rate: f64,
}

impl CrossoverResponse {
    /// The sum of the bands of a crossover at `frequencies`, run at
    /// `sample_rate`, with the limits the crossover applies: at most four
    /// frequencies, each limited to between 1 Hz and 0.49 of the sample
    /// rate. [`band`](Self::band) picks one band instead.
    pub fn new(slope: CrossoverSlope, frequencies: &[f64], sample_rate: f64) -> Self {
        let splits = frequencies.len().min(crossover::MAX_SPLITS);
        let mut gains = [1.0; crossover::MAX_SPLITS];
        for (gain, &hz) in gains.iter_mut().zip(&frequencies[..splits]) {
            *gain = crossover::integrator_gain(hz, sample_rate);
        }
        Self::from_gains(slope, &gains[..splits], sample_rate)
    }

    /// The sum of the bands of a crossover whose splits have the integrator
    /// gains `gains`.
    pub(crate) fn from_gains(slope: CrossoverSlope, gains: &[f64], sample_rate: f64) -> Self {
        let splits = gains.len().min(crossover::MAX_SPLITS);
        let mut all = [1.0; crossover::MAX_SPLITS];
        all[..splits].copy_from_slice(&gains[..splits]);
        Self {
            slope,
            splits,
            gains: all,
            band: None,
            sample_rate,
        }
    }

    /// The response of band `index`, from the lowest, or `None` for a band
    /// the crossover does not have.
    pub fn band(&self, index: usize) -> Option<Self> {
        (index <= self.splits).then_some(Self {
            band: Some(index),
            ..*self
        })
    }

    /// The response of the sum of the bands, which is what the crossover
    /// puts out as a [`Processor`](crate::Processor).
    #[must_use]
    pub fn sum(&self) -> Self {
        Self {
            band: None,
            ..*self
        }
    }

    /// The band this is the response of, or `None` for the sum.
    pub fn band_index(&self) -> Option<usize> {
        self.band
    }

    /// How many bands the crossover has.
    pub fn num_bands(&self) -> usize {
        self.splits + 1
    }

    /// The low band, the high band and the all-pass of split `index`, at
    /// the pre-warped frequency `t`.
    fn split(&self, index: usize, t: f64) -> [C; 3] {
        let w = t / self.gains[index];
        let s = C::new(0.0, w);
        let quadratic = |k: f64| C::new((1.0 - w) * (1.0 + w), k * w);
        let (butterworth, order) = match self.slope {
            CrossoverSlope::Lr2 => (ONE + s, 1),
            CrossoverSlope::Lr4 => (quadratic(SQRT_2), 2),
            CrossoverSlope::Lr8 => {
                let [k1, k2] = crossover::butterworth_4_damping();
                (quadratic(k1) * quadratic(k2), 4)
            }
        };
        // (-σ²)^N is w^(2N); halved, w^N / B.
        let low = divide(ONE, butterworth);
        let high = divide(C::new(w.powi(order), 0.0), butterworth);
        let all = divide(butterworth.conj(), butterworth);
        [low * low, high * high, all]
    }
}

impl FrequencyResponse for CrossoverResponse {
    fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    fn response_at(&self, frequency: f64) -> C {
        let t = prewarped(frequency, self.sample_rate);
        let mut product = ONE;
        for index in 0..self.splits {
            let [low, high, all] = self.split(index, t);
            product = product
                * match self.band {
                    None => all,
                    Some(band) if index < band => high,
                    Some(band) if index == band => low,
                    Some(_) => all,
                };
        }
        product
    }
}

/// The frequency response of a [`FirFilter`](crate::FirFilter): `Σ h[n]·e^(-jωn)`
/// over its taps `h`, evaluated by Horner's rule.
///
/// It borrows the taps, so it lives no longer than they do; an interface
/// that holds a copy of the taps can make one at any time.
///
/// ```
/// use manguebyte_dsp::curve::{FirResponse, FrequencyResponse};
///
/// // A three-tap moving average has a zero at a third of the sample rate.
/// let taps = [1.0 / 3.0; 3];
/// let response = FirResponse::new(&taps, 48_000.0);
/// assert!(response.magnitude_db_at(0.0).abs() < 1e-12);
/// assert!(response.magnitude_db_at(16_000.0) < -250.0);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FirResponse<'a, S: Sample = f64> {
    taps: &'a [S],
    sample_rate: f64,
}

impl<'a, S: Sample> FirResponse<'a, S> {
    /// The response of `taps`, run at `sample_rate`.
    pub fn new(taps: &'a [S], sample_rate: f64) -> Self {
        Self { taps, sample_rate }
    }

    /// The taps.
    pub fn taps(&self) -> &'a [S] {
        self.taps
    }
}

impl<S: Sample> FrequencyResponse for FirResponse<'_, S> {
    fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    fn response_at(&self, frequency: f64) -> C {
        let frequency = limit(frequency, 0.0, 0.5 * self.sample_rate);
        let (sin, cos) = (TAU * frequency / self.sample_rate).sin_cos();
        let delay = C::new(cos, -sin);
        self.taps
            .iter()
            .rev()
            .fold(C::ZERO, |sum, &tap| sum * delay + C::new(tap.to_f64(), 0.0))
    }
}

#[cfg(test)]
mod tests {
    use manguebyte_core::AudioBuffer;

    use super::*;
    use crate::curve::{Curve, MAX_DB, MIN_DB, Unit, gain_to_db, log_spaced, wrap};
    use crate::fft::RealFft;
    use crate::iir::{Band, Design};
    use crate::testing::{response_at, run_in_blocks, sine, tail_peak};
    use crate::{
        BiquadCascade, Crossover, Filter, FirFilter, FirMode, IirFilter, LadderFilter, OnePole,
        ProcessSpec, Processor, StateVariableFilter, fir,
    };

    const RATE: f64 = 48_000.0;

    fn spec(sample_rate: f64) -> ProcessSpec {
        ProcessSpec {
            sample_rate,
            max_block_size: 512,
            num_channels: 1,
        }
    }

    /// The impulse response of a prepared processor, to an impulse of
    /// `level`, scaled back to that of an impulse of 1 and checked to have
    /// died away within `len`.
    fn impulse_response<P: Processor<f64> + ?Sized>(
        processor: &mut P,
        len: usize,
        level: f64,
    ) -> Vec<f64> {
        let mut impulse = AudioBuffer::<f64>::new(1, len);
        impulse.channel_mut(0)[0] = level;
        run_in_blocks(processor, &mut impulse, 512);
        let response: Vec<f64> = impulse.channel(0).iter().map(|s| s / level).collect();
        assert!(tail_peak(&response, 256) < 1e-13, "the response is cut off");
        response
    }

    /// The largest difference between `curve` and the response measured
    /// from `impulse`, a power of two long, through the FFT: at about 200
    /// bins spread evenly in octaves from 10 Hz (or the first bin) to 0.45
    /// of the sample rate, in decibels where the curve is above -100 dB,
    /// and in radians where it is above -60 dB (below, the phase of a
    /// measurement is mostly rounding). The impulse response has died away
    /// within its length, so its spectrum is the response at the bins'
    /// frequencies.
    fn worst_error(curve: &impl FrequencyResponse, impulse: &[f64]) -> (f64, f64) {
        let rate = curve.sample_rate();
        let mut fft = RealFft::<f64>::new(impulse.len());
        let mut spectrum = vec![C::ZERO; fft.spectrum_len()];
        fft.forward(impulse, &mut spectrum);
        let bin_hz = rate / impulse.len() as f64;
        let mut hertz = [0.0; 200];
        log_spaced(10f64.max(bin_hz), 0.45 * rate, &mut hertz);
        let (mut worst_db, mut worst_phase) = (0.0f64, 0.0f64);
        for wanted in hertz {
            let bin = (wanted / bin_hz).round() as usize;
            let (frequency, measured) = (bin as f64 * bin_hz, spectrum[bin]);
            let expected = curve.magnitude_db_at(frequency);
            if expected > -100.0 {
                let db = gain_to_db(measured.re.hypot(measured.im));
                worst_db = worst_db.max((db - expected).abs());
            }
            if expected > -60.0 {
                let turn = wrap(measured.im.atan2(measured.re) - curve.phase_at(frequency));
                worst_phase = worst_phase.max(turn.abs());
            }
        }
        (worst_db, worst_phase)
    }

    /// Asserts that `curve` is what `impulse` measures, to within `db` and
    /// `radians`.
    fn assert_matches(
        what: &str,
        curve: &impl FrequencyResponse,
        impulse: &[f64],
        db: f64,
        radians: f64,
    ) {
        let (worst_db, worst_phase) = worst_error(curve, impulse);
        assert!(
            worst_db < db && worst_phase < radians,
            "{what}: off by {worst_db} dB and {worst_phase} rad"
        );
    }

    const SHAPES: [FilterShape; 11] = [
        FilterShape::Lowpass,
        FilterShape::Highpass,
        FilterShape::Bandpass,
        FilterShape::Notch,
        FilterShape::Allpass,
        FilterShape::Peak { gain_db: 9.0 },
        FilterShape::Peak { gain_db: -15.0 },
        FilterShape::LowShelf { gain_db: 6.0 },
        FilterShape::LowShelf { gain_db: -10.0 },
        FilterShape::HighShelf { gain_db: 4.0 },
        FilterShape::HighShelf { gain_db: -12.0 },
    ];

    #[test]
    fn a_biquad_runs_the_response_it_reports() {
        let mut worst = (0.0f64, 0.0f64);
        for shape in SHAPES {
            for (frequency, q) in [(60.0, 0.707), (1_000.0, 4.0), (12_000.0, 0.5)] {
                let mut filter = Filter::<f64>::new(shape, frequency, q);
                filter.prepare(&spec(RATE));
                let impulse = impulse_response(&mut filter, 1 << 16, 1.0);
                let curve = filter.response();
                let error = worst_error(&curve, &impulse);
                worst = (worst.0.max(error.0), worst.1.max(error.1));
                // From the settings alone, the same.
                assert_eq!(curve, BiquadResponse::design(shape, frequency, q, RATE));
            }
        }
        assert!(worst.0 < 1e-8 && worst.1 < 1e-8, "{worst:?}");
    }

    #[test]
    fn a_single_precision_biquad_reports_its_rounded_coefficients() {
        // A low-pass tuned low, where rounding to f32 moves the response by
        // most of a decibel below the cutoff.
        let (frequency, q, rate) = (20.0, 0.707, 192_000.0);
        let mut filter = Filter::<f32>::new(FilterShape::Lowpass, frequency, q);
        filter.prepare(&spec(rate));
        let curve = filter.response();
        let designed = BiquadResponse::design(FilterShape::Lowpass, frequency, q, rate);
        assert_eq!(curve, designed.rounded_to::<f32>());
        let moved = (curve.magnitude_db_at(1.0) - designed.magnitude_db_at(1.0)).abs();
        assert!(moved > 0.9 && moved < 1.0, "{moved}");

        // Measured on the f32 filter itself, through an impulse of 1: it
        // follows the rounded coefficients, not the design.
        let mut impulse = AudioBuffer::<f32>::new(1, 1 << 18);
        impulse.channel_mut(0)[0] = 1.0;
        run_in_blocks(&mut filter, &mut impulse, 512);
        let response: Vec<f64> = impulse.channel(0).iter().map(|&s| f64::from(s)).collect();
        for frequency in [1.0, 2.0, 5.0, 10.0] {
            let (re, im) = response_at(&response, frequency, rate);
            let measured = gain_to_db(re.hypot(im));
            let error = (measured - curve.magnitude_db_at(frequency)).abs();
            assert!(error < 0.05, "{frequency} Hz: {error} dB");
        }
    }

    fn designs() -> [(Design, f64, usize); 6] {
        [
            (
                Design::butterworth(Band::Lowpass { cutoff_hz: 1_000.0 }, 8),
                48_000.0,
                1 << 14,
            ),
            (
                Design::chebyshev_i(Band::Highpass { cutoff_hz: 200.0 }, 7, 1.0),
                44_100.0,
                1 << 16,
            ),
            (
                Design::chebyshev_ii(
                    Band::Bandpass {
                        low_hz: 500.0,
                        high_hz: 2_000.0,
                    },
                    5,
                    60.0,
                ),
                96_000.0,
                1 << 16,
            ),
            (
                Design::elliptic(Band::Lowpass { cutoff_hz: 5_000.0 }, 6, 0.5, 70.0),
                48_000.0,
                1 << 15,
            ),
            (
                Design::butterworth(
                    Band::Bandstop {
                        low_hz: 900.0,
                        high_hz: 1_100.0,
                    },
                    4,
                ),
                48_000.0,
                1 << 15,
            ),
            (
                Design::elliptic(
                    Band::Bandpass {
                        low_hz: 2_000.0,
                        high_hz: 3_000.0,
                    },
                    4,
                    1.0,
                    50.0,
                ),
                44_100.0,
                1 << 16,
            ),
        ]
    }

    #[test]
    fn a_cascade_runs_the_response_it_reports() {
        for (design, rate, len) in designs() {
            let mut filter = IirFilter::<f64>::new(design);
            filter.prepare(&spec(rate));
            let impulse = impulse_response(&mut filter, len, 1.0);
            let curve = filter.response();
            assert_eq!(curve, design.response(rate));
            assert_matches(&format!("{design:?}"), &curve, &impulse, 1e-8, 1e-8);

            // A cascade made from the coefficients runs those instead, and
            // reports them.
            let mut cascade = BiquadCascade::<f64>::new(&design.sections(rate));
            cascade.prepare(&spec(rate));
            let impulse = impulse_response(&mut cascade, len, 1.0);
            let reported = cascade.response(rate);
            assert_matches("from coefficients", &reported, &impulse, 1e-8, 1e-8);
            // And so do the coefficients themselves, one direct form each.
            let direct: Vec<BiquadResponse> = design
                .sections(rate)
                .iter()
                .map(|&section| BiquadResponse::new(section, rate))
                .collect();
            assert_matches("direct forms", &direct.as_slice(), &impulse, 1e-7, 1e-7);
        }
    }

    #[test]
    fn a_cascade_follows_the_design_where_its_coefficients_cannot() {
        // A type II low-pass at 10 Hz and 192 kHz with 200 dB of
        // attenuation: its coefficients are off by 14 dB (see the design
        // module), the form the cascade runs is not.
        let rate = 192_000.0;
        let design = Design::chebyshev_ii(Band::Lowpass { cutoff_hz: 10.0 }, 2, 200.0);
        let curve = design.response(rate);
        let coefficients: Vec<BiquadResponse> = design
            .sections(rate)
            .iter()
            .map(|&section| BiquadResponse::new(section, rate))
            .collect();
        let mut worst_curve = 0.0f64;
        let mut worst_coefficients = 0.0f64;
        let mut hertz = [0.0; 100];
        log_spaced(1e-5, 1.0, &mut hertz);
        for frequency in hertz {
            // Where the formula itself can be trusted.
            let formula = gain_to_db(design.magnitude_at(frequency, rate));
            if formula < -40.0 {
                continue;
            }
            worst_curve = worst_curve.max((curve.magnitude_db_at(frequency) - formula).abs());
            let direct = coefficients.as_slice().magnitude_db_at(frequency);
            worst_coefficients = worst_coefficients.max((direct - formula).abs());
        }
        assert!(worst_curve < 1e-9, "{worst_curve}");
        assert!(worst_coefficients > 10.0, "{worst_coefficients}");
    }

    #[test]
    fn a_state_variable_filter_runs_the_response_it_reports() {
        let modes = [
            SvfMode::Lowpass,
            SvfMode::Highpass,
            SvfMode::Bandpass,
            SvfMode::Notch,
            SvfMode::Allpass,
        ];
        for mode in modes {
            for (frequency, q) in [(40.0, 0.707), (1_000.0, 8.0), (15_000.0, 0.6)] {
                let mut filter = StateVariableFilter::<f64>::new(mode, frequency, q);
                filter.prepare(&spec(RATE));
                let impulse = impulse_response(&mut filter, 1 << 16, 1.0);
                let curve = filter.response();
                assert_eq!(curve, SvfResponse::new(mode, frequency, q, RATE));
                assert_matches(&format!("{mode:?}"), &curve, &impulse, 1e-8, 1e-8);
                // And the phase the filter reports itself, except at a
                // zero, where neither means anything.
                for hz in [10.0, 500.0, 1_000.0, 20_000.0] {
                    if curve.magnitude_db_at(hz) < -200.0 {
                        continue;
                    }
                    let turn = wrap(curve.phase_at(hz) - filter.phase_response(hz));
                    assert!(turn.abs() < 1e-12, "{mode:?} at {hz}: {turn}");
                }
            }
        }
    }

    #[test]
    fn a_one_pole_runs_the_response_it_reports() {
        for mode in [
            OnePoleMode::Lowpass,
            OnePoleMode::Highpass,
            OnePoleMode::Allpass,
        ] {
            for frequency in [20.0, 1_000.0, 15_000.0] {
                let mut filter = OnePole::<f64>::new(mode, frequency);
                filter.prepare(&spec(RATE));
                let impulse = impulse_response(&mut filter, 1 << 15, 1.0);
                let curve = filter.response();
                assert_eq!(curve, OnePoleResponse::new(mode, frequency, RATE));
                assert_matches(&format!("{mode:?}"), &curve, &impulse, 1e-8, 1e-8);
            }
        }
    }

    #[test]
    fn a_quiet_ladder_runs_the_response_of_its_linear_model() {
        for mode in LadderMode::ALL {
            for (frequency, resonance, drive_db) in [
                (300.0, 0.0, 0.0),
                (2_000.0, 0.5, 12.0),
                (6_000.0, 0.9, -6.0),
            ] {
                let mut filter = LadderFilter::<f64>::new(mode, frequency)
                    .with_resonance(resonance)
                    .with_drive_db(drive_db);
                filter.prepare(&spec(RATE));
                // So quiet that the saturator is a wire.
                let impulse = impulse_response(&mut filter, 1 << 15, 1e-7);
                let curve = filter.response();
                let settings = LadderResponse::new(mode, frequency, resonance, drive_db, RATE);
                assert_eq!(curve, settings);
                assert_matches(&format!("{mode:?}"), &curve, &impulse, 1e-7, 1e-7);
            }
        }
    }

    #[test]
    fn every_band_of_a_crossover_and_their_sum_run_the_responses_reported() {
        const LEN: usize = 1 << 16;
        let splits: [&[f64]; 3] = [
            &[1_000.0],
            &[200.0, 2_000.0],
            &[80.0, 400.0, 2_500.0, 9_000.0],
        ];
        for slope in CrossoverSlope::ALL {
            for frequencies in splits {
                let mut crossover = Crossover::<f64>::new(slope, frequencies);
                crossover.prepare(&ProcessSpec {
                    sample_rate: RATE,
                    max_block_size: LEN,
                    num_channels: 1,
                });
                let mut impulse = AudioBuffer::<f64>::new(1, LEN);
                impulse.channel_mut(0)[0] = 1.0;
                crossover.split(&impulse);
                let curve = crossover.response();
                assert_eq!(curve, CrossoverResponse::new(slope, frequencies, RATE));
                assert_eq!(curve.num_bands(), frequencies.len() + 1);
                let mut sum = vec![0.0; LEN];
                for band in 0..crossover.num_bands() {
                    let measured = crossover.band(band).channel(0).to_vec();
                    for (total, value) in sum.iter_mut().zip(&measured) {
                        *total += value;
                    }
                    let reported = crossover.band_response(band).unwrap();
                    assert_eq!(reported.band_index(), Some(band));
                    let what = format!("{slope:?} {frequencies:?}, band {band}");
                    assert_matches(&what, &reported, &measured, 1e-7, 1e-7);
                }
                assert_matches("the sum", &curve, &sum, 1e-7, 1e-7);
                assert!(crossover.band_response(frequencies.len() + 1).is_none());
            }
        }
    }

    #[test]
    fn a_fir_filter_runs_the_response_it_reports() {
        let taps = fir::lowpass(255, 0.1, 80.0);
        for mode in [FirMode::Direct, FirMode::Fft] {
            let mut filter = FirFilter::<f64>::with_mode(&taps, mode).unwrap();
            filter.prepare(&spec(RATE));
            let mut impulse = AudioBuffer::<f64>::new(1, 1_024);
            impulse.channel_mut(0)[0] = 1.0;
            run_in_blocks(&mut filter, &mut impulse, 100);
            let curve = filter.response(RATE);
            assert_matches(&format!("{mode:?}"), &curve, impulse.channel(0), 1e-8, 1e-8);
            // The same as the taps the filter was given.
            for frequency in [0.0, 1_000.0, 4_800.0, 10_000.0] {
                let ours = curve.response_at(frequency);
                let theirs = FirResponse::new(&taps, RATE).response_at(frequency);
                assert!((ours - theirs).norm() < 1e-15);
            }
        }
        // Single precision rounds the taps, and the response says so.
        let filter = FirFilter::<f32>::new(&taps).unwrap();
        let rounded: Vec<f32> = taps.iter().map(|&tap| tap as f32).collect();
        assert_eq!(filter.response(RATE).taps(), rounded.as_slice());
    }

    #[test]
    fn a_chain_of_bands_adds_up_their_decibels_and_their_phases() {
        let mut bands = [
            Filter::<f64>::new(FilterShape::LowShelf { gain_db: 4.0 }, 120.0, 0.707),
            Filter::<f64>::new(FilterShape::Peak { gain_db: -6.0 }, 900.0, 2.0),
            Filter::<f64>::new(FilterShape::Peak { gain_db: 3.0 }, 4_000.0, 1.0),
            Filter::<f64>::new(FilterShape::HighShelf { gain_db: -5.0 }, 9_000.0, 0.707),
        ];
        for band in &mut bands {
            band.prepare(&spec(RATE));
        }
        let [a, b, c, d] = bands.clone();
        let mut chain = (a, b, c, d);
        chain.prepare(&spec(RATE));
        let impulse = impulse_response(&mut chain, 1 << 16, 1.0);
        let responses = bands.each_ref().map(Filter::response);
        assert_matches("an array", &responses, &impulse, 1e-8, 1e-8);
        assert_matches("a slice", &responses.as_slice(), &impulse, 1e-8, 1e-8);
        let [a, b, c, d] = responses;
        assert_matches("a tuple", &(a, b, c, d), &impulse, 1e-8, 1e-8);
        // Mixed kinds of filter in a chain, through references to `dyn`.
        let svf = SvfResponse::new(SvfMode::Highpass, 30.0, 0.707, RATE);
        let dynamic: [&dyn FrequencyResponse; 2] = [&responses, &svf];
        for frequency in [20.0, 900.0, 4_000.0] {
            let sum: f64 = responses.iter().map(|r| r.magnitude_db_at(frequency)).sum();
            assert!((responses.magnitude_db_at(frequency) - sum).abs() < 1e-12);
            let both = sum + svf.magnitude_db_at(frequency);
            assert!((dynamic.as_slice().magnitude_db_at(frequency) - both).abs() < 1e-12);
            let product = responses.response_at(frequency);
            assert!((20.0 * product.norm().log10() - sum).abs() < 1e-9);
        }
        // An empty chain passes everything.
        let none: [BiquadResponse; 0] = [];
        assert_eq!(none.magnitude_db_at(1_000.0), 0.0);
        assert_eq!(none.phase_at(1_000.0), 0.0);
        assert_eq!(none.sample_rate(), 44_100.0);
    }

    /// The complex response to a steady sine at `frequency`, measured over
    /// the last half of a second of it after a second to settle, by least
    /// squares against a sine and a cosine.
    fn sine_response<P: Processor<f64>>(processor: &mut P, frequency: f64, level: f64) -> C {
        let len = (1.5 * RATE) as usize;
        let mut signal = sine::<f64>(1, len, frequency, RATE, level);
        run_in_blocks(processor, &mut signal, 512);
        let omega = TAU * frequency / RATE;
        let (mut ss, mut sc, mut cc, mut ys, mut yc) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for (n, &y) in signal.channel(0).iter().enumerate().skip(len - len / 3) {
            let (s, c) = (omega * n as f64).sin_cos();
            ss += s * s;
            sc += s * c;
            cc += c * c;
            ys += y * s;
            yc += y * c;
        }
        let determinant = ss * cc - sc * sc;
        let a = (ys * cc - yc * sc) / determinant;
        let b = (yc * ss - ys * sc) / determinant;
        C::new(a / level, b / level)
    }

    #[test]
    fn steady_sines_come_out_as_the_curves_say() {
        let check = |what: &str, measured: C, curve: &dyn FrequencyResponse, frequency: f64| {
            let error_db = (gain_to_db(measured.norm()) - curve.magnitude_db_at(frequency)).abs();
            let error_phase = wrap(measured.arg() - curve.phase_at(frequency)).abs();
            assert!(
                error_db < 1e-6 && error_phase < 1e-6,
                "{what} at {frequency} Hz: {error_db} dB, {error_phase} rad"
            );
        };
        for frequency in [100.0, 1_000.0, 5_000.0, 15_000.0] {
            let mut filter = Filter::<f64>::new(FilterShape::Peak { gain_db: 6.0 }, 2_000.0, 1.5);
            filter.prepare(&spec(RATE));
            let curve = filter.response();
            let measured = sine_response(&mut filter, frequency, 0.5);
            check("peak", measured, &curve, frequency);

            let mut svf = StateVariableFilter::<f64>::new(SvfMode::Bandpass, 3_000.0, 3.0);
            svf.prepare(&spec(RATE));
            let curve = svf.response();
            let measured = sine_response(&mut svf, frequency, 0.5);
            check("band-pass", measured, &curve, frequency);

            let mut ladder =
                LadderFilter::<f64>::new(LadderMode::Lp24, 4_000.0).with_resonance(0.7);
            ladder.prepare(&spec(RATE));
            let curve = ladder.response();
            let measured = sine_response(&mut ladder, frequency, 1e-7);
            check("ladder", measured, &curve, frequency);
        }
    }

    #[test]
    fn the_curves_are_finite_at_the_edges_and_for_garbage() {
        let notch = BiquadResponse::design(FilterShape::Notch, 1_000.0, 2.0, RATE);
        let high = Design::butterworth(Band::Highpass { cutoff_hz: 100.0 }, 6).response(RATE);
        let svf = SvfResponse::new(SvfMode::Lowpass, f64::NAN, f64::NAN, RATE);
        let one_pole = OnePoleResponse::new(OnePoleMode::Highpass, -5.0, RATE);
        let ladder = LadderResponse::new(LadderMode::Hp24, 1e9, 2.0, f64::NAN, RATE);
        let crossover = CrossoverResponse::new(CrossoverSlope::Lr8, &[f64::NAN, 1e9], RATE);
        let fir = FirResponse::<f64>::new(&[], RATE);
        let responses: [&dyn FrequencyResponse; 7] =
            [&notch, &high, &svf, &one_pole, &ladder, &crossover, &fir];
        for response in responses {
            for frequency in [
                0.0,
                1_000.0,
                0.5 * RATE,
                RATE,
                -1.0,
                f64::NAN,
                f64::INFINITY,
            ] {
                let db = response.magnitude_db_at(frequency);
                let phase = response.phase_at(frequency);
                assert!((MIN_DB..=MAX_DB).contains(&db), "{db}");
                assert!(phase.is_finite() && phase.abs() <= PI + 1e-12, "{phase}");
            }
        }
        // A notch's centre is a gain of zero, drawn at the floor.
        let notch = SvfResponse::new(SvfMode::Notch, 1_000.0, 2.0, RATE);
        assert!(notch.magnitude_db_at(1_000.0) < -250.0);
        // A pole hit exactly, at the edge of self-oscillation, at the top.
        let ringing = LadderResponse::new(LadderMode::Lp24, 1_000.0, 1.0, 0.0, RATE);
        assert!(ringing.magnitude_db_at(1_000.0) > 250.0);
        // A sample rate that is garbage gives numbers too.
        let broken = SvfResponse::new(SvfMode::Lowpass, 1_000.0, 1.0, f64::NAN);
        assert_eq!(broken.magnitude_db_at(100.0), MIN_DB);
        assert_eq!(broken.magnitude().spec().x.max, 20_000.0);
    }

    #[test]
    fn the_range_of_a_magnitude_curve_fits_it() {
        let eq = BiquadResponse::design(FilterShape::Peak { gain_db: 4.0 }, 1_000.0, 1.0, RATE);
        let spec = eq.magnitude().spec();
        assert_eq!((spec.y.min, spec.y.max), (-12.0, 12.0));
        let low = Design::butterworth(Band::Lowpass { cutoff_hz: 1_000.0 }, 2).response(RATE);
        let spec = low.magnitude().spec();
        // 75 dB down at 20 kHz.
        assert_eq!(spec.y.max, 6.0);
        assert_eq!(spec.y.min, -78.0);
        let steep = Design::butterworth(Band::Lowpass { cutoff_hz: 100.0 }, 16).response(RATE);
        assert_eq!(steep.magnitude().spec().y.min, -120.0);
        let resonant = SvfResponse::new(SvfMode::Lowpass, 1_000.0, 10.0, RATE);
        assert_eq!(resonant.magnitude().spec().y.max, 24.0);
        let phase = resonant.phase().spec();
        assert_eq!(phase.y.unit, Unit::Radians);
        assert_eq!(resonant.phase().value_at(10.0), resonant.phase_at(10.0));
    }
}
