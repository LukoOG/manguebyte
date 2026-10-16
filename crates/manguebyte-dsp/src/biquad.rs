use core::f64::consts::TAU;

use manguebyte_core::{AudioBuffer, Sample};

use crate::curve::BiquadResponse;
use crate::{ProcessSpec, Processor};

/// The response a second-order filter is designed for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FilterShape {
    /// Passes frequencies below the cutoff.
    Lowpass,
    /// Passes frequencies above the cutoff.
    Highpass,
    /// Passes a band around the centre frequency, with unity gain at the peak.
    Bandpass,
    /// Rejects a narrow band around the centre frequency.
    Notch,
    /// Passes everything, shifting phase around the centre frequency.
    Allpass,
    /// Boosts or cuts a band around the centre frequency.
    Peak {
        /// Gain at the centre frequency, in decibels.
        gain_db: f64,
    },
    /// Boosts or cuts everything below the corner frequency.
    LowShelf {
        /// Gain of the shelf, in decibels.
        gain_db: f64,
    },
    /// Boosts or cuts everything above the corner frequency.
    HighShelf {
        /// Gain of the shelf, in decibels.
        gain_db: f64,
    },
}

/// Coefficients of a second-order section, normalised so that `a0 == 1`:
///
/// ```text
/// y[n] = b0*x[n] + b1*x[n-1] + b2*x[n-2] - a1*y[n-1] - a2*y[n-2]
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BiquadCoefficients {
    /// Feed-forward coefficient for `x[n]`.
    pub b0: f64,
    /// Feed-forward coefficient for `x[n-1]`.
    pub b1: f64,
    /// Feed-forward coefficient for `x[n-2]`.
    pub b2: f64,
    /// Feedback coefficient for `y[n-1]`.
    pub a1: f64,
    /// Feedback coefficient for `y[n-2]`.
    pub a2: f64,
}

impl BiquadCoefficients {
    /// A filter that leaves the signal untouched.
    pub const IDENTITY: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    /// Designs a filter using Robert Bristow-Johnson's Audio EQ Cookbook
    /// formulas.
    ///
    /// `frequency` is clamped to just inside `0..sample_rate / 2`, and `q` to
    /// a small positive minimum, so the result is always a stable filter.
    pub fn design(shape: FilterShape, sample_rate: f64, frequency: f64, q: f64) -> Self {
        let frequency = frequency.clamp(1e-3, sample_rate * 0.499);
        let q = q.max(1e-3);
        let w0 = TAU * frequency / sample_rate;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);

        let (b0, b1, b2, a0, a1, a2) = match shape {
            FilterShape::Lowpass => (
                (1.0 - cos) / 2.0,
                1.0 - cos,
                (1.0 - cos) / 2.0,
                1.0 + alpha,
                -2.0 * cos,
                1.0 - alpha,
            ),
            FilterShape::Highpass => (
                (1.0 + cos) / 2.0,
                -(1.0 + cos),
                (1.0 + cos) / 2.0,
                1.0 + alpha,
                -2.0 * cos,
                1.0 - alpha,
            ),
            FilterShape::Bandpass => (alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
            FilterShape::Notch => (1.0, -2.0 * cos, 1.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
            FilterShape::Allpass => (
                1.0 - alpha,
                -2.0 * cos,
                1.0 + alpha,
                1.0 + alpha,
                -2.0 * cos,
                1.0 - alpha,
            ),
            FilterShape::Peak { gain_db } => {
                let a = 10f64.powf(gain_db / 40.0);
                (
                    1.0 + alpha * a,
                    -2.0 * cos,
                    1.0 - alpha * a,
                    1.0 + alpha / a,
                    -2.0 * cos,
                    1.0 - alpha / a,
                )
            }
            FilterShape::LowShelf { gain_db } => {
                let a = 10f64.powf(gain_db / 40.0);
                let beta = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cos + beta),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                    a * ((a + 1.0) - (a - 1.0) * cos - beta),
                    (a + 1.0) + (a - 1.0) * cos + beta,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                    (a + 1.0) + (a - 1.0) * cos - beta,
                )
            }
            FilterShape::HighShelf { gain_db } => {
                let a = 10f64.powf(gain_db / 40.0);
                let beta = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cos + beta),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                    a * ((a + 1.0) + (a - 1.0) * cos - beta),
                    (a + 1.0) - (a - 1.0) * cos + beta,
                    2.0 * ((a - 1.0) - (a + 1.0) * cos),
                    (a + 1.0) - (a - 1.0) * cos - beta,
                )
            }
        };

        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    /// Linear gain of the filter at `frequency`. Useful for drawing response
    /// curves.
    pub fn magnitude_at(&self, frequency: f64, sample_rate: f64) -> f64 {
        let w = TAU * frequency / sample_rate;
        let (sin1, cos1) = w.sin_cos();
        let (sin2, cos2) = (2.0 * w).sin_cos();

        let num_re = self.b0 + self.b1 * cos1 + self.b2 * cos2;
        let num_im = -(self.b1 * sin1 + self.b2 * sin2);
        let den_re = 1.0 + self.a1 * cos1 + self.a2 * cos2;
        let den_im = -(self.a1 * sin1 + self.a2 * sin2);

        ((num_re * num_re + num_im * num_im) / (den_re * den_re + den_im * den_im)).sqrt()
    }

    /// The coefficients rounded to the precision of `S`, as a
    /// [`Biquad<S>`](Biquad) holds them.
    #[must_use]
    pub fn rounded_to<S: Sample>(&self) -> Self {
        let round = |c: f64| S::from_f64(c).to_f64();
        Self {
            b0: round(self.b0),
            b1: round(self.b1),
            b2: round(self.b2),
            a1: round(self.a1),
            a2: round(self.a2),
        }
    }
}

/// A second-order filter section driven by raw [`BiquadCoefficients`].
///
/// Reach for [`Filter`] when you want to think in terms of shape, frequency
/// and Q; use this when you design coefficients yourself.
#[derive(Debug, Clone)]
pub struct Biquad<S: Sample = f32> {
    b0: S,
    b1: S,
    b2: S,
    a1: S,
    a2: S,
    /// Transposed direct form II state, one pair per channel.
    state: Vec<[S; 2]>,
}

impl<S: Sample> Biquad<S> {
    /// Creates a section with the given coefficients.
    pub fn new(coefficients: BiquadCoefficients) -> Self {
        let mut biquad = Self {
            b0: S::ONE,
            b1: S::ZERO,
            b2: S::ZERO,
            a1: S::ZERO,
            a2: S::ZERO,
            state: Vec::new(),
        };
        biquad.set_coefficients(coefficients);
        biquad
    }

    /// Replaces the coefficients, keeping the filter memory.
    pub fn set_coefficients(&mut self, coefficients: BiquadCoefficients) {
        self.b0 = S::from_f64(coefficients.b0);
        self.b1 = S::from_f64(coefficients.b1);
        self.b2 = S::from_f64(coefficients.b2);
        self.a1 = S::from_f64(coefficients.a1);
        self.a2 = S::from_f64(coefficients.a2);
    }

    /// The coefficients in use, in the precision of `S` they are held in.
    pub fn coefficients(&self) -> BiquadCoefficients {
        BiquadCoefficients {
            b0: self.b0.to_f64(),
            b1: self.b1.to_f64(),
            b2: self.b2.to_f64(),
            a1: self.a1.to_f64(),
            a2: self.a2.to_f64(),
        }
    }

    /// The frequency response of the section run at `sample_rate`, from
    /// the coefficients in use, for drawing. See [`curve`](crate::curve).
    pub fn response(&self, sample_rate: f64) -> BiquadResponse {
        BiquadResponse::new(self.coefficients(), sample_rate)
    }
}

impl<S: Sample> Processor<S> for Biquad<S> {
    fn prepare(&mut self, spec: &ProcessSpec) {
        self.state.clear();
        self.state.resize(spec.num_channels, [S::ZERO; 2]);
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        debug_assert!(
            buffer.num_channels() <= self.state.len(),
            "buffer has more channels than the filter was prepared for"
        );
        let (b0, b1, b2, a1, a2) = (self.b0, self.b1, self.b2, self.a1, self.a2);
        for (channel, state) in buffer.iter_channels_mut().zip(&mut self.state) {
            let [mut z1, mut z2] = *state;
            for sample in channel {
                let x = *sample;
                let y = b0 * x + z1;
                z1 = b1 * x - a1 * y + z2;
                z2 = b2 * x - a2 * y;
                *sample = y;
            }
            *state = [z1, z2];
        }
    }

    fn reset(&mut self) {
        self.state.fill([S::ZERO; 2]);
    }
}

/// A second-order filter controlled by shape, frequency and Q.
///
/// Changing a setting recomputes the coefficients immediately, which is cheap
/// enough to do on the audio thread once per block.
#[derive(Debug, Clone)]
pub struct Filter<S: Sample = f32> {
    shape: FilterShape,
    frequency: f64,
    q: f64,
    sample_rate: f64,
    biquad: Biquad<S>,
}

impl<S: Sample> Filter<S> {
    /// Creates a filter. Until [`prepare`](Processor::prepare) is called it
    /// assumes 44.1 kHz.
    pub fn new(shape: FilterShape, frequency: f64, q: f64) -> Self {
        let sample_rate = 44_100.0;
        Self {
            shape,
            frequency,
            q,
            sample_rate,
            biquad: Biquad::new(BiquadCoefficients::design(shape, sample_rate, frequency, q)),
        }
    }

    /// Changes the response shape.
    pub fn set_shape(&mut self, shape: FilterShape) {
        self.shape = shape;
        self.update();
    }

    /// Changes the cutoff or centre frequency, in hertz.
    pub fn set_frequency(&mut self, frequency: f64) {
        self.frequency = frequency;
        self.update();
    }

    /// Changes the resonance. `0.707` gives a maximally flat low- or highpass.
    pub fn set_q(&mut self, q: f64) {
        self.q = q;
        self.update();
    }

    /// The coefficients currently in use.
    pub fn coefficients(&self) -> BiquadCoefficients {
        BiquadCoefficients::design(self.shape, self.sample_rate, self.frequency, self.q)
    }

    /// The frequency response the filter runs, for drawing: that of its
    /// coefficients as they are held, in the precision of `S`, at the
    /// sample rate it was prepared for. See [`curve`](crate::curve).
    ///
    /// ```
    /// use manguebyte_dsp::curve::FrequencyResponse;
    /// use manguebyte_dsp::{Filter, FilterShape, ProcessSpec, Processor};
    ///
    /// let mut filter = Filter::<f32>::new(FilterShape::Lowpass, 1_000.0, 0.707);
    /// filter.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 64, num_channels: 1 });
    /// assert!((filter.response().magnitude_db_at(1_000.0) + 3.01).abs() < 0.01);
    /// ```
    pub fn response(&self) -> BiquadResponse {
        self.biquad.response(self.sample_rate)
    }

    fn update(&mut self) {
        self.biquad.set_coefficients(self.coefficients());
    }
}

impl<S: Sample> Processor<S> for Filter<S> {
    fn prepare(&mut self, spec: &ProcessSpec) {
        self.sample_rate = spec.sample_rate;
        self.update();
        self.biquad.prepare(spec);
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        self.biquad.process(buffer);
    }

    fn reset(&mut self) {
        self.biquad.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RATE: f64 = 48_000.0;
    const BUTTERWORTH_Q: f64 = core::f64::consts::FRAC_1_SQRT_2;

    fn design(shape: FilterShape, frequency: f64, q: f64) -> BiquadCoefficients {
        BiquadCoefficients::design(shape, SAMPLE_RATE, frequency, q)
    }

    fn assert_close(actual: f64, expected: f64, tolerance: f64) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn lowpass_response() {
        let c = design(FilterShape::Lowpass, 1_000.0, BUTTERWORTH_Q);
        assert_close(c.magnitude_at(10.0, SAMPLE_RATE), 1.0, 1e-3);
        assert_close(c.magnitude_at(1_000.0, SAMPLE_RATE), BUTTERWORTH_Q, 1e-6);
        // Second order: roughly -12 dB per octave above the cutoff.
        assert!(c.magnitude_at(8_000.0, SAMPLE_RATE) < 0.02);
    }

    #[test]
    fn highpass_response() {
        let c = design(FilterShape::Highpass, 1_000.0, BUTTERWORTH_Q);
        assert!(c.magnitude_at(100.0, SAMPLE_RATE) < 0.02);
        assert_close(c.magnitude_at(1_000.0, SAMPLE_RATE), BUTTERWORTH_Q, 1e-6);
        assert_close(c.magnitude_at(20_000.0, SAMPLE_RATE), 1.0, 1e-2);
    }

    #[test]
    fn bandpass_notch_and_allpass_response() {
        let band = design(FilterShape::Bandpass, 2_000.0, 4.0);
        assert_close(band.magnitude_at(2_000.0, SAMPLE_RATE), 1.0, 1e-9);
        assert!(band.magnitude_at(200.0, SAMPLE_RATE) < 0.1);

        let notch = design(FilterShape::Notch, 2_000.0, 4.0);
        assert!(notch.magnitude_at(2_000.0, SAMPLE_RATE) < 1e-9);
        assert_close(notch.magnitude_at(100.0, SAMPLE_RATE), 1.0, 1e-2);

        let allpass = design(FilterShape::Allpass, 2_000.0, 1.0);
        for frequency in [50.0, 2_000.0, 15_000.0] {
            assert_close(allpass.magnitude_at(frequency, SAMPLE_RATE), 1.0, 1e-9);
        }
    }

    #[test]
    fn peak_and_shelves_hit_their_gain() {
        let gain = 10f64.powf(6.0 / 20.0);

        let peak = design(FilterShape::Peak { gain_db: 6.0 }, 1_000.0, 1.0);
        assert_close(peak.magnitude_at(1_000.0, SAMPLE_RATE), gain, 1e-9);
        assert_close(peak.magnitude_at(20.0, SAMPLE_RATE), 1.0, 1e-2);

        let low = design(FilterShape::LowShelf { gain_db: 6.0 }, 500.0, BUTTERWORTH_Q);
        assert_close(low.magnitude_at(10.0, SAMPLE_RATE), gain, 1e-2);
        assert_close(low.magnitude_at(20_000.0, SAMPLE_RATE), 1.0, 1e-2);

        let high = design(
            FilterShape::HighShelf { gain_db: -6.0 },
            4_000.0,
            BUTTERWORTH_Q,
        );
        assert_close(high.magnitude_at(20.0, SAMPLE_RATE), 1.0, 1e-2);
        assert_close(high.magnitude_at(23_000.0, SAMPLE_RATE), 1.0 / gain, 1e-2);
    }

    #[test]
    fn identity_passes_the_signal_through() {
        let mut biquad = Biquad::<f64>::new(BiquadCoefficients::IDENTITY);
        biquad.prepare(&ProcessSpec {
            sample_rate: SAMPLE_RATE,
            max_block_size: 4,
            num_channels: 1,
        });
        let mut buffer = AudioBuffer::<f64>::new(1, 4);
        buffer
            .channel_mut(0)
            .copy_from_slice(&[1.0, -0.5, 0.25, 0.0]);
        biquad.process(&mut buffer);
        assert_eq!(buffer.channel(0), &[1.0, -0.5, 0.25, 0.0]);
    }

    /// Runs a sine through the filter and measures the gain once it settles.
    fn measured_gain(filter: &mut Filter<f64>, frequency: f64) -> f64 {
        const FRAMES: usize = 9_600;
        filter.prepare(&ProcessSpec {
            sample_rate: SAMPLE_RATE,
            max_block_size: FRAMES,
            num_channels: 1,
        });
        let mut buffer = AudioBuffer::<f64>::new(1, FRAMES);
        for (n, sample) in buffer.channel_mut(0).iter_mut().enumerate() {
            *sample = (TAU * frequency * n as f64 / SAMPLE_RATE).sin();
        }
        filter.process(&mut buffer);
        let settled = &buffer.channel(0)[FRAMES / 2..];
        settled.iter().fold(0.0f64, |peak, s| peak.max(s.abs()))
    }

    #[test]
    fn processing_matches_the_designed_response() {
        let mut filter = Filter::<f64>::new(FilterShape::Lowpass, 1_000.0, BUTTERWORTH_Q);
        for frequency in [100.0, 1_000.0, 6_000.0] {
            filter.reset();
            let expected = filter.coefficients().magnitude_at(frequency, SAMPLE_RATE);
            let measured = measured_gain(&mut filter, frequency);
            assert_close(measured, expected, 5e-3);
        }
    }

    #[test]
    fn channels_keep_separate_state() {
        let mut filter = Filter::<f32>::new(FilterShape::Lowpass, 500.0, BUTTERWORTH_Q);
        filter.prepare(&ProcessSpec {
            sample_rate: SAMPLE_RATE,
            max_block_size: 32,
            num_channels: 2,
        });
        let mut buffer = AudioBuffer::<f32>::new(2, 32);
        buffer.channel_mut(0).fill(1.0);
        filter.process(&mut buffer);
        assert!(buffer.channel(0)[31] > 0.0);
        assert_eq!(buffer.channel(1), &[0.0; 32]);
    }

    #[test]
    fn reset_clears_the_tail() {
        let mut filter = Filter::<f32>::new(FilterShape::Lowpass, 500.0, 4.0);
        filter.prepare(&ProcessSpec {
            sample_rate: SAMPLE_RATE,
            max_block_size: 32,
            num_channels: 1,
        });
        let mut buffer = AudioBuffer::<f32>::new(1, 32);
        buffer.channel_mut(0).fill(1.0);
        filter.process(&mut buffer);

        filter.reset();
        buffer.clear();
        filter.process(&mut buffer);
        assert_eq!(buffer.peak(), 0.0);
    }
}
