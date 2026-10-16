//! The inverse modified discrete cosine transform, by way of a complex FFT
//! a quarter of the block long.
//!
//! For a block of `n` samples and `m = n / 2` coefficients `X`, the
//! transform of section 1.3.2 of the Vorbis I specification is
//!
//! ```text
//! y[i] = sum over k of X[k] cos(2 pi / n (i + 1/2 + n/4) (k + 1/2)),  i = 0 .. n
//! ```
//!
//! It is worked out here in three steps, each a standard identity:
//!
//! 1. `y` is a type IV discrete cosine transform `u` of `X`, read with its
//!    symmetries: `y[i] = u[i + m/2]` for the first quarter, `-u[3m/2 - 1 - i]`
//!    for the middle half and `-u[i - 3m/2]` for the last quarter.
//! 2. With `c[p] = X[2p] + i X[m - 1 - 2p]`, the sum
//!    `Z[j] = sum over p of c[p] exp(-i pi / m (2j + 1/2)(2p + 1/2))` gives
//!    `u[2j] = Re Z[j]` and `u[m - 1 - 2j] = -Im Z[j]`.
//! 3. The exponent splits into `2 pi j p / (m/2)`, which is an FFT of `m / 2`
//!    points, and two twiddles, `exp(-i pi p / m)` before it and
//!    `exp(-i pi (j + 1/4) / m)` after.
//!
//! The FFT is the radix-2 decimation in time of Cooley and Tukey (1965).

use std::f64::consts::PI;

/// A complex number, in `f32` like the samples.
#[derive(Debug, Clone, Copy, Default)]
struct Complex {
    re: f32,
    im: f32,
}

impl Complex {
    fn mul(self, other: Self) -> Self {
        Self {
            re: self.re * other.re - self.im * other.im,
            im: self.re * other.im + self.im * other.re,
        }
    }
}

/// The inverse transform for one block size, with its tables.
#[derive(Debug)]
pub(super) struct Imdct {
    n: usize,
    /// `exp(-i pi p / m)` for each of the `m / 2` inputs of the FFT.
    before: Vec<Complex>,
    /// `exp(-i pi (j + 1/4) / m)` for each output of the FFT.
    after: Vec<Complex>,
    /// `exp(-2 pi i k / (m / 2))` for the first half of the FFT's length.
    roots: Vec<Complex>,
    /// Where each input of the FFT goes: its index with the bits reversed.
    order: Vec<u32>,
    work: Vec<Complex>,
}

impl Imdct {
    /// The transform for blocks of `n` samples, a power of two from 64.
    pub(super) fn new(n: usize) -> Self {
        let m = n / 2;
        let len = m / 2;
        let twiddle = |angle: f64| Complex {
            re: angle.cos() as f32,
            im: angle.sin() as f32,
        };
        let before = (0..len)
            .map(|p| twiddle(-PI * p as f64 / m as f64))
            .collect();
        let after = (0..len)
            .map(|j| twiddle(-PI * (j as f64 + 0.25) / m as f64))
            .collect();
        let roots = (0..len / 2)
            .map(|k| twiddle(-2.0 * PI * k as f64 / len as f64))
            .collect();
        let bits = len.trailing_zeros();
        let order = (0..len as u32)
            .map(|index| index.reverse_bits() >> (32 - bits))
            .collect();
        Self {
            n,
            before,
            after,
            roots,
            order,
            work: vec![Complex::default(); len],
        }
    }

    /// Transforms the `n / 2` coefficients in `input` into the `n` samples of
    /// `output`.
    pub(super) fn inverse(&mut self, input: &[f32], output: &mut [f32]) {
        let n = self.n;
        let m = n / 2;
        let len = m / 2;
        debug_assert!(input.len() >= m && output.len() >= n);

        // Fold the coefficients into complex numbers, twiddle them and put
        // them in the order the FFT takes them.
        for p in 0..len {
            let value = Complex {
                re: input[2 * p],
                im: input[m - 1 - 2 * p],
            };
            self.work[self.order[p] as usize] = value.mul(self.before[p]);
        }

        // The FFT, in place.
        let mut size = 2;
        while size <= len {
            let half = size / 2;
            let stride = len / size;
            for start in (0..len).step_by(size) {
                for k in 0..half {
                    let root = self.roots[k * stride];
                    let a = self.work[start + k];
                    let b = self.work[start + k + half].mul(root);
                    self.work[start + k] = Complex {
                        re: a.re + b.re,
                        im: a.im + b.im,
                    };
                    self.work[start + k + half] = Complex {
                        re: a.re - b.re,
                        im: a.im - b.im,
                    };
                }
            }
            size *= 2;
        }

        // Twiddle again; the real and imaginary parts are the even and the
        // odd values of the type IV transform, the odd ones backwards.
        // `output` holds `u` in its first half for the moment.
        for j in 0..len {
            let z = self.work[j].mul(self.after[j]);
            output[2 * j] = z.re;
            output[m - 1 - 2 * j] = -z.im;
        }

        // Unfold `u` into the block. The last quarter and the second half
        // of the middle read the lower half of `u`, so they are written
        // first, into the upper half of the block, which is free; the first
        // quarter and the first half of the middle read the upper half of
        // `u`, which stays where it is until they have.
        let (first, rest) = output.split_at_mut(m);
        let quarter = m / 2;
        for t in 0..quarter {
            // y[3m/2 + t] = -u[t]
            rest[quarter + t] = -first[t];
        }
        for t in 0..quarter {
            // y[m + t] = -u[m/2 - 1 - t], which was just written above.
            rest[t] = rest[m - 1 - t];
        }
        for t in 0..quarter {
            // y[t] = u[m/2 + t]
            first[t] = first[quarter + t];
        }
        // y[m/2 + t] = -u[m - 1 - t]: the upper half of `u`, backwards.
        let middle = &mut first[quarter..];
        middle.reverse();
        for value in middle {
            *value = -*value;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The transform as the specification writes it, one sum per sample.
    fn direct(input: &[f32]) -> Vec<f64> {
        let n = input.len() * 2;
        (0..n)
            .map(|i| {
                input
                    .iter()
                    .enumerate()
                    .map(|(k, &x)| {
                        let angle = 2.0 * PI / n as f64
                            * (i as f64 + 0.5 + n as f64 / 4.0)
                            * (k as f64 + 0.5);
                        f64::from(x) * angle.cos()
                    })
                    .sum()
            })
            .collect()
    }

    #[test]
    fn the_fast_transform_is_the_direct_one() {
        let mut state = 0x1234_5678u32;
        for n in [64, 128, 256, 2_048] {
            let input: Vec<f32> = (0..n / 2)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    (state as f32 / u32::MAX as f32) - 0.5
                })
                .collect();
            let mut output = vec![0.0; n];
            Imdct::new(n).inverse(&input, &mut output);
            let expected = direct(&input);
            let scale = expected
                .iter()
                .fold(0.0f64, |peak, &value| peak.max(value.abs()));
            for (i, (&got, &want)) in output.iter().zip(&expected).enumerate() {
                assert!(
                    (f64::from(got) - want).abs() < 1e-5 * scale,
                    "n {n}, sample {i}: {got} against {want}"
                );
            }
        }
    }
}
