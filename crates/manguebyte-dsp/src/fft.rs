//! The fast Fourier transform.
//!
//! - [`Fft`] transforms complex signals of any length.
//! - [`RealFft`] transforms real signals, which is what audio is, at about
//!   half the cost and producing only the non-redundant half of the spectrum.
//!
//! A plan is built once for a given length and reused. Building allocates;
//! transforming does not, so plans can be used on the audio thread.
//!
//! ```
//! use manguebyte_dsp::fft::{Complex, RealFft};
//!
//! // One second of a 5 Hz sine sampled at 64 Hz.
//! let signal: Vec<f32> = (0..64)
//!     .map(|n| (std::f32::consts::TAU * 5.0 * n as f32 / 64.0).sin())
//!     .collect();
//!
//! let mut fft = RealFft::<f32>::new(64);
//! let mut spectrum = vec![Complex::ZERO; fft.spectrum_len()];
//! fft.forward(&signal, &mut spectrum);
//!
//! // All the energy is in bin 5.
//! let loudest = (0..spectrum.len())
//!     .max_by(|&a, &b| spectrum[a].norm_sqr().total_cmp(&spectrum[b].norm_sqr()))
//!     .unwrap();
//! assert_eq!(loudest, 5);
//! ```

use core::f64::consts::PI;
use core::ops::{Add, Mul, Neg, Sub};

use manguebyte_core::Sample;

/// A complex number.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Complex<S = f32> {
    /// Real part.
    pub re: S,
    /// Imaginary part.
    pub im: S,
}

impl<S: Sample> Complex<S> {
    /// Zero.
    pub const ZERO: Self = Self {
        re: S::ZERO,
        im: S::ZERO,
    };

    /// Creates a complex number from its real and imaginary parts.
    #[inline]
    pub const fn new(re: S, im: S) -> Self {
        Self { re, im }
    }

    /// Creates a complex number from a length and an angle in radians.
    #[inline]
    pub fn from_polar(magnitude: f64, phase: f64) -> Self {
        Self {
            re: S::from_f64(magnitude * phase.cos()),
            im: S::from_f64(magnitude * phase.sin()),
        }
    }

    /// The complex conjugate.
    #[inline]
    pub fn conj(self) -> Self {
        Self {
            re: self.re,
            im: -self.im,
        }
    }

    /// The squared magnitude. Cheaper than [`norm`](Self::norm) when only
    /// comparing or summing power.
    #[inline]
    pub fn norm_sqr(self) -> S {
        self.re * self.re + self.im * self.im
    }

    /// The magnitude.
    #[inline]
    pub fn norm(self) -> S {
        self.norm_sqr().sqrt()
    }

    /// The angle in radians, in `-π..=π`.
    #[inline]
    pub fn arg(self) -> S {
        S::from_f64(self.im.to_f64().atan2(self.re.to_f64()))
    }

    /// Multiplies both parts by a real number.
    #[inline]
    pub fn scale(self, factor: S) -> Self {
        Self {
            re: self.re * factor,
            im: self.im * factor,
        }
    }
}

impl<S: Sample> Add for Complex<S> {
    type Output = Self;
    #[inline]
    fn add(self, other: Self) -> Self {
        Self {
            re: self.re + other.re,
            im: self.im + other.im,
        }
    }
}

impl<S: Sample> Sub for Complex<S> {
    type Output = Self;
    #[inline]
    fn sub(self, other: Self) -> Self {
        Self {
            re: self.re - other.re,
            im: self.im - other.im,
        }
    }
}

impl<S: Sample> Mul for Complex<S> {
    type Output = Self;
    #[inline]
    fn mul(self, other: Self) -> Self {
        Self {
            re: self.re * other.re - self.im * other.im,
            im: self.re * other.im + self.im * other.re,
        }
    }
}

impl<S: Sample> Neg for Complex<S> {
    type Output = Self;
    #[inline]
    fn neg(self) -> Self {
        Self {
            re: -self.re,
            im: -self.im,
        }
    }
}

/// A power-of-two transform: iterative radix-2 with precomputed twiddle
/// factors and bit-reversal table.
#[derive(Debug, Clone)]
struct Radix2<S> {
    /// `twiddles[k] = e^(-2πik/N)` for `k` in `0..N/2`.
    twiddles: Vec<Complex<S>>,
    /// For each index, the index with its bits reversed.
    reversed: Vec<u32>,
}

impl<S: Sample> Radix2<S> {
    fn new(len: usize) -> Self {
        debug_assert!(len.is_power_of_two());
        let bits = len.trailing_zeros();
        Self {
            twiddles: (0..len / 2)
                .map(|k| Complex::from_polar(1.0, -2.0 * PI * k as f64 / len as f64))
                .collect(),
            reversed: (0..len as u32)
                .map(|index| {
                    if bits == 0 {
                        0
                    } else {
                        index.reverse_bits() >> (u32::BITS - bits)
                    }
                })
                .collect(),
        }
    }

    fn len(&self) -> usize {
        self.reversed.len()
    }

    /// Transforms in place, unscaled. `inverse` flips the sign of the
    /// exponent.
    fn transform(&self, data: &mut [Complex<S>], inverse: bool) {
        let len = self.len();
        debug_assert_eq!(data.len(), len);
        for (index, &reversed) in self.reversed.iter().enumerate() {
            let reversed = reversed as usize;
            if index < reversed {
                data.swap(index, reversed);
            }
        }

        let mut half = 1;
        while half < len {
            let stride = len / (2 * half);
            for block in data.chunks_exact_mut(2 * half) {
                let (low, high) = block.split_at_mut(half);
                for (k, (a, b)) in low.iter_mut().zip(high).enumerate() {
                    let twiddle = self.twiddles[k * stride];
                    let twiddle = if inverse { twiddle.conj() } else { twiddle };
                    let rotated = *b * twiddle;
                    *b = *a - rotated;
                    *a = *a + rotated;
                }
            }
            half *= 2;
        }
    }
}

/// Bluestein's algorithm: a transform of any length expressed as a
/// convolution, which is carried out with a power-of-two transform.
///
/// L. Bluestein, "A linear filtering approach to the computation of discrete
/// Fourier transform", IEEE Transactions on Audio and Electroacoustics, 1970.
#[derive(Debug, Clone)]
struct Bluestein<S> {
    inner: Radix2<S>,
    /// `chirp[n] = e^(-πin²/N)`.
    chirp: Vec<Complex<S>>,
    /// Transform of the conjugated, wrapped chirp, scaled by `1/M`.
    kernel: Vec<Complex<S>>,
    scratch: Vec<Complex<S>>,
}

impl<S: Sample> Bluestein<S> {
    fn new(len: usize) -> Self {
        let padded = (2 * len - 1).next_power_of_two();
        let inner = Radix2::new(padded);
        // n² is reduced modulo 2N before it becomes an angle, so the phase
        // stays accurate however long the transform is.
        let chirp: Vec<Complex<S>> = (0..len as u64)
            .map(|n| {
                let squared = (n * n) % (2 * len as u64);
                Complex::from_polar(1.0, -PI * squared as f64 / len as f64)
            })
            .collect();

        let mut kernel = vec![Complex::ZERO; padded];
        kernel[0] = chirp[0].conj();
        for n in 1..len {
            kernel[n] = chirp[n].conj();
            kernel[padded - n] = chirp[n].conj();
        }
        inner.transform(&mut kernel, false);
        let scale = S::from_f64(1.0 / padded as f64);
        for value in &mut kernel {
            *value = value.scale(scale);
        }

        Self {
            inner,
            chirp,
            kernel,
            scratch: vec![Complex::ZERO; padded],
        }
    }

    fn transform(&mut self, data: &mut [Complex<S>], inverse: bool) {
        // The inverse is the forward transform of the conjugate, conjugated.
        let prepare = |value: Complex<S>| if inverse { value.conj() } else { value };

        let len = data.len();
        for ((slot, &value), &chirp) in self.scratch.iter_mut().zip(data.iter()).zip(&self.chirp) {
            *slot = prepare(value) * chirp;
        }
        self.scratch[len..].fill(Complex::ZERO);

        self.inner.transform(&mut self.scratch, false);
        for (value, &kernel) in self.scratch.iter_mut().zip(&self.kernel) {
            *value = *value * kernel;
        }
        self.inner.transform(&mut self.scratch, true);

        for ((out, &value), &chirp) in data.iter_mut().zip(&self.scratch).zip(&self.chirp) {
            *out = prepare(value * chirp);
        }
    }
}

#[derive(Debug, Clone)]
enum Plan<S> {
    Radix2(Radix2<S>),
    Bluestein(Bluestein<S>),
}

/// A fast Fourier transform of complex signals of one fixed length.
///
/// Any length works. Powers of two are the fastest; other lengths cost
/// roughly three power-of-two transforms of the next size up.
///
/// The forward transform is unscaled and the inverse divides by the length,
/// so a forward transform followed by an inverse returns the original signal.
#[derive(Debug, Clone)]
pub struct Fft<S: Sample = f32> {
    len: usize,
    plan: Plan<S>,
}

impl<S: Sample> Fft<S> {
    /// Plans a transform of `len` points. Allocates.
    ///
    /// # Panics
    ///
    /// Panics if `len` is zero.
    pub fn new(len: usize) -> Self {
        assert!(len > 0, "an FFT needs at least one point");
        let plan = if len.is_power_of_two() {
            Plan::Radix2(Radix2::new(len))
        } else {
            Plan::Bluestein(Bluestein::new(len))
        };
        Self { len, plan }
    }

    /// The number of points this plan transforms.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Transforms from time to frequency, in place. Real-time safe.
    ///
    /// # Panics
    ///
    /// Panics if `data` does not hold exactly [`len`](Self::len) points.
    pub fn forward(&mut self, data: &mut [Complex<S>]) {
        self.transform(data, false);
    }

    /// Transforms from frequency to time, in place, dividing by the length.
    /// Real-time safe.
    ///
    /// # Panics
    ///
    /// Panics if `data` does not hold exactly [`len`](Self::len) points.
    pub fn inverse(&mut self, data: &mut [Complex<S>]) {
        self.transform(data, true);
        let scale = S::from_f64(1.0 / self.len as f64);
        for value in data {
            *value = value.scale(scale);
        }
    }

    fn transform(&mut self, data: &mut [Complex<S>], inverse: bool) {
        assert_eq!(
            data.len(),
            self.len,
            "this FFT was planned for {} points",
            self.len
        );
        match &mut self.plan {
            Plan::Radix2(plan) => plan.transform(data, inverse),
            Plan::Bluestein(plan) => plan.transform(data, inverse),
        }
    }
}

/// A fast Fourier transform of real signals of one fixed length.
///
/// A real signal of `N` samples has a spectrum whose upper half mirrors its
/// lower half, so only bins `0..=N/2` are produced: bin `k` is the frequency
/// `k * sample_rate / N`.
///
/// For even lengths the work is done by a complex transform of half the
/// size. The scaling matches [`Fft`]: forward is unscaled, inverse divides by
/// the length.
#[derive(Debug, Clone)]
pub struct RealFft<S: Sample = f32> {
    len: usize,
    fft: Fft<S>,
    /// Even lengths only: `e^(-2πik/N)` for `k` in `0..=N/2`.
    twiddles: Vec<Complex<S>>,
    scratch: Vec<Complex<S>>,
}

impl<S: Sample> RealFft<S> {
    /// Plans a transform of `len` real samples. Allocates.
    ///
    /// # Panics
    ///
    /// Panics if `len` is zero.
    pub fn new(len: usize) -> Self {
        assert!(len > 0, "an FFT needs at least one point");
        if len % 2 == 0 {
            let half = len / 2;
            Self {
                len,
                fft: Fft::new(half),
                twiddles: (0..=half)
                    .map(|k| Complex::from_polar(1.0, -2.0 * PI * k as f64 / len as f64))
                    .collect(),
                scratch: vec![Complex::ZERO; half],
            }
        } else {
            Self {
                len,
                fft: Fft::new(len),
                twiddles: Vec::new(),
                scratch: vec![Complex::ZERO; len],
            }
        }
    }

    /// The number of real samples this plan transforms.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.len
    }

    /// The number of frequency bins: `len / 2 + 1`.
    pub fn spectrum_len(&self) -> usize {
        self.len / 2 + 1
    }

    /// Transforms `input` into its spectrum. Real-time safe.
    ///
    /// # Panics
    ///
    /// Panics unless `input` holds [`len`](Self::len) samples and `spectrum`
    /// holds [`spectrum_len`](Self::spectrum_len) bins.
    pub fn forward(&mut self, input: &[S], spectrum: &mut [Complex<S>]) {
        self.check(input.len(), spectrum.len());
        if self.twiddles.is_empty() {
            for (slot, &sample) in self.scratch.iter_mut().zip(input) {
                *slot = Complex::new(sample, S::ZERO);
            }
            self.fft.forward(&mut self.scratch);
            spectrum.copy_from_slice(&self.scratch[..spectrum.len()]);
            return;
        }

        // Pack even samples into the real part and odd samples into the
        // imaginary part, transform at half size, then separate the two
        // interleaved spectra again.
        let half = self.len / 2;
        for (slot, pair) in self.scratch.iter_mut().zip(input.chunks_exact(2)) {
            *slot = Complex::new(pair[0], pair[1]);
        }
        self.fft.forward(&mut self.scratch);

        let half_scale = S::from_f64(0.5);
        for (k, bin) in spectrum.iter_mut().enumerate() {
            let direct = self.scratch[k % half];
            let mirrored = self.scratch[(half - k) % half].conj();
            let even = (direct + mirrored).scale(half_scale);
            let difference = (direct - mirrored).scale(half_scale);
            // odd = -i * difference
            let odd = Complex::new(difference.im, -difference.re);
            *bin = even + self.twiddles[k] * odd;
        }
    }

    /// Transforms a spectrum back into real samples, dividing by the length.
    /// Real-time safe.
    ///
    /// # Panics
    ///
    /// Panics unless `spectrum` holds [`spectrum_len`](Self::spectrum_len)
    /// bins and `output` holds [`len`](Self::len) samples.
    pub fn inverse(&mut self, spectrum: &[Complex<S>], output: &mut [S]) {
        self.check(output.len(), spectrum.len());
        if self.twiddles.is_empty() {
            // Rebuild the mirrored upper half, then transform at full size.
            let bins = spectrum.len();
            self.scratch[..bins].copy_from_slice(spectrum);
            for k in bins..self.len {
                self.scratch[k] = spectrum[self.len - k].conj();
            }
            self.fft.inverse(&mut self.scratch);
            for (sample, value) in output.iter_mut().zip(&self.scratch) {
                *sample = value.re;
            }
            return;
        }

        let half = self.len / 2;
        let half_scale = S::from_f64(0.5);
        for (k, slot) in self.scratch.iter_mut().enumerate() {
            let direct = spectrum[k];
            let mirrored = spectrum[half - k].conj();
            let even = (direct + mirrored).scale(half_scale);
            let odd = (direct - mirrored).scale(half_scale) * self.twiddles[k].conj();
            // even + i * odd
            *slot = Complex::new(even.re - odd.im, even.im + odd.re);
        }
        self.fft.inverse(&mut self.scratch);
        for (pair, value) in output.chunks_exact_mut(2).zip(&self.scratch) {
            pair[0] = value.re;
            pair[1] = value.im;
        }
    }

    fn check(&self, samples: usize, bins: usize) {
        assert_eq!(
            samples, self.len,
            "this FFT was planned for {} samples",
            self.len
        );
        assert_eq!(
            bins,
            self.spectrum_len(),
            "the spectrum of {} samples has {} bins",
            self.len,
            self.spectrum_len()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LENGTHS: [usize; 14] = [1, 2, 3, 4, 5, 7, 8, 12, 17, 64, 100, 127, 256, 441];

    /// A deterministic signal with no special structure.
    fn noise(len: usize, seed: u64) -> Vec<f64> {
        let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
            })
            .collect()
    }

    fn complex_noise(len: usize) -> Vec<Complex<f64>> {
        noise(len, 1)
            .into_iter()
            .zip(noise(len, 2))
            .map(|(re, im)| Complex::new(re, im))
            .collect()
    }

    /// The transform straight from its definition.
    fn dft(input: &[Complex<f64>]) -> Vec<Complex<f64>> {
        let len = input.len();
        (0..len)
            .map(|k| {
                input
                    .iter()
                    .enumerate()
                    .fold(Complex::ZERO, |sum, (n, &value)| {
                        let phase = -2.0 * PI * ((k * n) % len) as f64 / len as f64;
                        sum + value * Complex::from_polar(1.0, phase)
                    })
            })
            .collect()
    }

    fn max_error(a: &[Complex<f64>], b: &[Complex<f64>]) -> f64 {
        a.iter()
            .zip(b)
            .map(|(&x, &y)| (x - y).norm())
            .fold(0.0, f64::max)
    }

    #[test]
    fn matches_the_definition_at_every_length() {
        for len in LENGTHS {
            let input = complex_noise(len);
            let mut output = input.clone();
            Fft::new(len).forward(&mut output);
            let error = max_error(&output, &dft(&input));
            assert!(error < 1e-10 * len as f64, "length {len}: error {error}");
        }
    }

    #[test]
    fn inverse_undoes_forward() {
        for len in LENGTHS {
            let input = complex_noise(len);
            let mut data = input.clone();
            let mut fft = Fft::new(len);
            fft.forward(&mut data);
            fft.inverse(&mut data);
            assert!(max_error(&data, &input) < 1e-12, "length {len}");
        }
    }

    #[test]
    fn an_impulse_has_a_flat_spectrum() {
        let mut data = vec![Complex::<f32>::ZERO; 16];
        data[0] = Complex::new(1.0, 0.0);
        Fft::new(16).forward(&mut data);
        for bin in data {
            assert!((bin.re - 1.0).abs() < 1e-6 && bin.im.abs() < 1e-6);
        }
    }

    #[test]
    fn energy_is_preserved() {
        for len in [64, 100] {
            let input = complex_noise(len);
            let mut spectrum = input.clone();
            Fft::new(len).forward(&mut spectrum);
            let time: f64 = input.iter().map(|v| v.norm_sqr()).sum();
            let frequency: f64 = spectrum.iter().map(|v| v.norm_sqr()).sum::<f64>() / len as f64;
            assert!((time - frequency).abs() < 1e-9 * time, "length {len}");
        }
    }

    #[test]
    fn real_transform_matches_the_complex_one() {
        for len in LENGTHS {
            let signal = noise(len, 3);
            let mut expected: Vec<Complex<f64>> =
                signal.iter().map(|&s| Complex::new(s, 0.0)).collect();
            Fft::new(len).forward(&mut expected);

            let mut fft = RealFft::new(len);
            let mut spectrum = vec![Complex::ZERO; fft.spectrum_len()];
            fft.forward(&signal, &mut spectrum);
            let error = max_error(&spectrum, &expected[..spectrum.len()]);
            assert!(error < 1e-10 * len as f64, "length {len}: error {error}");

            let mut restored = vec![0.0; len];
            fft.inverse(&spectrum, &mut restored);
            for (a, b) in restored.iter().zip(&signal) {
                assert!((a - b).abs() < 1e-12, "length {len}");
            }
        }
    }

    #[test]
    fn a_sine_lands_in_its_bin_with_half_the_length_as_magnitude() {
        let len = 256;
        let signal: Vec<f32> = (0..len)
            .map(|n| (core::f32::consts::TAU * 10.0 * n as f32 / len as f32).sin())
            .collect();
        let mut fft = RealFft::<f32>::new(len);
        let mut spectrum = vec![Complex::ZERO; fft.spectrum_len()];
        fft.forward(&signal, &mut spectrum);

        for (bin, value) in spectrum.iter().enumerate() {
            let expected = if bin == 10 { len as f32 / 2.0 } else { 0.0 };
            assert!(
                (value.norm() - expected).abs() < 1e-3,
                "bin {bin}: {}",
                value.norm()
            );
        }
        // A sine is a cosine delayed by a quarter turn.
        assert!((spectrum[10].arg() + core::f32::consts::FRAC_PI_2).abs() < 1e-4);
    }

    #[test]
    fn single_precision_round_trip_is_accurate() {
        for len in [512, 480] {
            let signal: Vec<f32> = noise(len, 4).into_iter().map(|s| s as f32).collect();
            let mut fft = RealFft::<f32>::new(len);
            let mut spectrum = vec![Complex::ZERO; fft.spectrum_len()];
            let mut restored = vec![0.0f32; len];
            fft.forward(&signal, &mut spectrum);
            fft.inverse(&spectrum, &mut restored);
            for (a, b) in restored.iter().zip(&signal) {
                assert!((a - b).abs() < 2e-5, "length {len}: {a} vs {b}");
            }
        }
    }

    #[test]
    #[should_panic(expected = "planned for 8 points")]
    fn refuses_the_wrong_length() {
        Fft::<f32>::new(8).forward(&mut [Complex::ZERO; 4]);
    }
}
