//! Window functions.
//!
//! Cutting a block out of a signal and transforming it smears every frequency
//! across the whole spectrum, because the block's abrupt ends look like a
//! sudden change that was never in the sound. Multiplying the block by a
//! window that fades to nothing at both ends trades that smearing for a
//! slightly wider peak.
//!
//! ```
//! use manguebyte_dsp::window::{Symmetry, Window};
//!
//! let mut block = [1.0f32; 8];
//! Window::Hann.apply(&mut block, Symmetry::Periodic);
//! assert_eq!(block[0], 0.0);
//! assert_eq!(block[4], 1.0);
//! ```

use core::f64::consts::PI;

use manguebyte_core::Sample;

/// Which of the two conventional lengths a window is computed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Symmetry {
    /// One period of a shape that repeats every `len` samples. The right
    /// choice for spectral analysis and for overlap-add processing, where
    /// consecutive windows must sum to a constant.
    Periodic,
    /// A shape whose first and last samples mirror each other. The right
    /// choice for designing FIR filters.
    Symmetric,
}

/// A window shape.
///
/// The sums of cosines use the coefficients from F. J. Harris, "On the use of
/// windows for harmonic analysis with the discrete Fourier transform",
/// Proceedings of the IEEE, 1978, and A. H. Nuttall, "Some windows with very
/// good sidelobe behavior", IEEE Transactions on Acoustics, Speech, and
/// Signal Processing, 1981.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Window {
    /// No shaping. The narrowest peak and by far the most leakage.
    Rectangular,
    /// A straight rise and fall, reaching zero at the ends.
    Triangular,
    /// A raised cosine. The usual first choice: leakage falls away quickly
    /// and overlapped copies sum to a constant.
    Hann,
    /// A raised cosine on a small pedestal. Lower nearest sidelobe than Hann,
    /// but distant leakage does not keep falling.
    Hamming,
    /// Three cosine terms. Sidelobes around 58 dB down.
    Blackman,
    /// Four cosine terms. Sidelobes around 92 dB down, for measuring quiet
    /// components next to loud ones.
    BlackmanHarris,
    /// Four cosine terms with sidelobes around 93 dB down that keep falling.
    Nuttall,
    /// Five cosine terms with a flat top, so a sine reads at its true level
    /// wherever it falls between two bins. For measuring amplitude.
    FlatTop,
    /// The Kaiser window. The parameter trades peak width for leakage: `0`
    /// is rectangular, `5` is close to Hamming, `9` close to Blackman-Harris.
    Kaiser(f64),
}

impl Window {
    /// The window's value at `index` out of `len` samples.
    ///
    /// # Panics
    ///
    /// Panics if `index >= len`.
    pub fn value(self, index: usize, len: usize, symmetry: Symmetry) -> f64 {
        assert!(index < len, "index {index} is outside a window of {len}");
        let period = match symmetry {
            Symmetry::Periodic => len,
            Symmetry::Symmetric => len - 1,
        };
        if period == 0 {
            // A symmetric window of one sample is just its peak.
            return 1.0;
        }
        // Position across the window, from 0 up to (but for periodic windows
        // not reaching) 1.
        let position = index as f64 / period as f64;
        let cosines = |terms: &[f64]| -> f64 {
            terms
                .iter()
                .enumerate()
                .map(|(order, &term)| {
                    let sign = if order % 2 == 0 { 1.0 } else { -1.0 };
                    sign * term * (2.0 * PI * order as f64 * position).cos()
                })
                .sum()
        };
        match self {
            Self::Rectangular => 1.0,
            Self::Triangular => 1.0 - (2.0 * position - 1.0).abs(),
            Self::Hann => cosines(&[0.5, 0.5]),
            Self::Hamming => cosines(&[0.54, 0.46]),
            Self::Blackman => cosines(&[0.42, 0.5, 0.08]),
            Self::BlackmanHarris => cosines(&[0.358_75, 0.488_29, 0.141_28, 0.011_68]),
            Self::Nuttall => cosines(&[0.355_768, 0.487_396, 0.144_232, 0.012_604]),
            Self::FlatTop => cosines(&[
                0.215_578_95,
                0.416_631_58,
                0.277_263_158,
                0.083_578_947,
                0.006_947_368,
            ]),
            Self::Kaiser(beta) => {
                let offset = 2.0 * position - 1.0;
                bessel_i0(beta * (1.0 - offset * offset).max(0.0).sqrt()) / bessel_i0(beta)
            }
        }
    }

    /// Writes the window into `out`.
    pub fn fill<S: Sample>(self, out: &mut [S], symmetry: Symmetry) {
        let len = out.len();
        for (index, value) in out.iter_mut().enumerate() {
            *value = S::from_f64(self.value(index, len, symmetry));
        }
    }

    /// Multiplies `block` by the window.
    ///
    /// This evaluates the window as it goes, which is too slow for the audio
    /// thread. There, [`fill`](Self::fill) a table once and multiply by it.
    pub fn apply<S: Sample>(self, block: &mut [S], symmetry: Symmetry) {
        let len = block.len();
        for (index, sample) in block.iter_mut().enumerate() {
            *sample *= S::from_f64(self.value(index, len, symmetry));
        }
    }

    /// The window's average value: how much it attenuates a steady sine.
    /// Divide a measured spectrum peak by `len * coherent_gain / 2` to read
    /// the sine's amplitude.
    pub fn coherent_gain(self, len: usize, symmetry: Symmetry) -> f64 {
        if len == 0 {
            return 0.0;
        }
        (0..len)
            .map(|index| self.value(index, len, symmetry))
            .sum::<f64>()
            / len as f64
    }
}

/// The zeroth-order modified Bessel function of the first kind, from its
/// power series. Converges quickly for the arguments a Kaiser window uses.
fn bessel_i0(x: f64) -> f64 {
    let quarter_square = x * x / 4.0;
    let mut term = 1.0;
    let mut sum = 1.0;
    for k in 1..200 {
        term *= quarter_square / (k * k) as f64;
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fft::{Complex, RealFft};

    const ALL: [Window; 9] = [
        Window::Rectangular,
        Window::Triangular,
        Window::Hann,
        Window::Hamming,
        Window::Blackman,
        Window::BlackmanHarris,
        Window::Nuttall,
        Window::FlatTop,
        Window::Kaiser(8.0),
    ];

    fn table(window: Window, len: usize, symmetry: Symmetry) -> Vec<f64> {
        let mut out = vec![0.0; len];
        window.fill(&mut out, symmetry);
        out
    }

    #[test]
    fn symmetric_windows_mirror_and_peak_in_the_middle() {
        for window in ALL {
            for len in [1, 2, 9, 64] {
                let values = table(window, len, Symmetry::Symmetric);
                for (a, b) in values.iter().zip(values.iter().rev()) {
                    assert!((a - b).abs() < 1e-12, "{window:?} of {len}");
                }
                let peak = values.iter().copied().fold(f64::MIN, f64::max);
                assert!((peak - values[len / 2]).abs() < 1e-9, "{window:?} of {len}");
                // The published flat-top coefficients add up to 1.000000003.
                assert!(peak <= 1.0 + 1e-8, "{window:?} peaks at {peak}");
            }
        }
    }

    #[test]
    fn periodic_window_is_the_longer_symmetric_one_without_its_last_sample() {
        for window in ALL {
            let periodic = table(window, 32, Symmetry::Periodic);
            let symmetric = table(window, 33, Symmetry::Symmetric);
            for (a, b) in periodic.iter().zip(&symmetric) {
                assert!((a - b).abs() < 1e-12, "{window:?}");
            }
        }
    }

    #[test]
    fn known_values() {
        assert_eq!(Window::Hann.value(0, 8, Symmetry::Periodic), 0.0);
        assert!((Window::Hann.value(2, 8, Symmetry::Periodic) - 0.5).abs() < 1e-12);
        assert!((Window::Hamming.value(0, 9, Symmetry::Symmetric) - 0.08).abs() < 1e-12);
        assert!(Window::Blackman.value(0, 9, Symmetry::Symmetric).abs() < 1e-12);
        assert!((Window::Triangular.value(2, 9, Symmetry::Symmetric) - 0.5).abs() < 1e-12);
        assert!((Window::Hann.coherent_gain(64, Symmetry::Periodic) - 0.5).abs() < 1e-12);
        assert!((Window::Hamming.coherent_gain(64, Symmetry::Periodic) - 0.54).abs() < 1e-12);
    }

    #[test]
    fn kaiser_with_zero_beta_is_rectangular_and_narrows_as_beta_grows() {
        for value in table(Window::Kaiser(0.0), 16, Symmetry::Symmetric) {
            assert!((value - 1.0).abs() < 1e-12);
        }
        let gentle = Window::Kaiser(2.0).coherent_gain(64, Symmetry::Symmetric);
        let steep = Window::Kaiser(10.0).coherent_gain(64, Symmetry::Symmetric);
        assert!(steep < gentle);
        // I0(1) from Abramowitz and Stegun, table 9.8.
        assert!((bessel_i0(1.0) - 1.266_065_877_752_008).abs() < 1e-14);
    }

    #[test]
    fn overlapped_hann_windows_sum_to_a_constant() {
        let len = 64;
        let hop = len / 2;
        let window = table(Window::Hann, len, Symmetry::Periodic);
        for offset in 0..hop {
            assert!((window[offset] + window[offset + hop] - 1.0).abs() < 1e-12);
        }
    }

    /// Level of the highest sidelobe relative to the main peak, in decibels,
    /// measured on a heavily zero-padded transform.
    fn highest_sidelobe_db(window: Window) -> f64 {
        let len = 64;
        let padded = 4096;
        let mut signal = vec![0.0f64; padded];
        window.fill(&mut signal[..len], Symmetry::Symmetric);
        let mut fft = RealFft::new(padded);
        let mut spectrum = vec![Complex::ZERO; fft.spectrum_len()];
        fft.forward(&signal, &mut spectrum);
        let magnitudes: Vec<f64> = spectrum.iter().map(|bin| bin.norm()).collect();

        // Walk down the main lobe to its first minimum, then take the largest
        // value beyond it.
        let mut edge = 1;
        while edge + 1 < magnitudes.len() && magnitudes[edge + 1] < magnitudes[edge] {
            edge += 1;
        }
        let sidelobe = magnitudes[edge..].iter().copied().fold(0.0, f64::max);
        20.0 * (sidelobe / magnitudes[0]).log10()
    }

    #[test]
    fn sidelobes_are_where_the_literature_says() {
        for (window, expected) in [
            (Window::Rectangular, -13.3),
            (Window::Hann, -31.5),
            (Window::Hamming, -42.7),
            (Window::Blackman, -58.1),
            (Window::BlackmanHarris, -92.0),
            (Window::Nuttall, -93.3),
        ] {
            let measured = highest_sidelobe_db(window);
            assert!(
                (measured - expected).abs() < 1.5,
                "{window:?}: {measured:.1} dB, expected about {expected} dB"
            );
        }
    }
}
