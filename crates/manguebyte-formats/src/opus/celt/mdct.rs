//! The inverse MDCT of CELT with its low-overlap window, and the FFT it is
//! worked out with.
//!
//! The transform follows the structure the reference implementation gives
//! it, since the scale and the phase of what comes out depend on it: the
//! `N / 2` coefficients are folded into `N / 4` complex values and rotated,
//! put through a forward FFT of `N / 4` points, rotated back, and unfolded
//! around the window, whose overlap with the block before is added in the
//! same step (a 2 by 2 rotation per pair of samples, the time-domain
//! aliasing cancellation of Princen and Bradley, 1986).
//!
//! The FFT is the mixed-radix decimation in time of Cooley and Tukey
//! (1965), with radices 2, 3, 4 and 5, which the frame sizes of CELT need:
//! 60, 120, 240 and 480 points.

use std::f64::consts::PI;
use std::sync::OnceLock;

use super::tables::{MAX_LM, OVERLAP, SHORT_MDCT};

#[derive(Debug, Clone, Copy, Default)]
struct Complex {
    re: f32,
    im: f32,
}

impl Complex {
    #[inline]
    fn mul(self, other: Self) -> Self {
        Self {
            re: self.re * other.re - self.im * other.im,
            im: self.re * other.im + self.im * other.re,
        }
    }

    #[inline]
    fn add(self, other: Self) -> Self {
        Self {
            re: self.re + other.re,
            im: self.im + other.im,
        }
    }
}

/// `exp(-2 pi i k / n)`.
fn root(k: usize, n: usize) -> Complex {
    let angle = -2.0 * PI * k as f64 / n as f64;
    Complex {
        re: angle.cos() as f32,
        im: angle.sin() as f32,
    }
}

/// A forward FFT of one size.
struct Fft {
    /// The radix of each stage, from the outermost.
    factors: Vec<usize>,
    /// For each stage, of `n` points split into `radix` transforms of `m`,
    /// the twiddle of sub-transform `r` at bin `u`, `exp(-2 pi i r u / n)`,
    /// at `u * (radix - 1) + r - 1`.
    twiddles: Vec<Vec<Complex>>,
    /// The roots of unity of the radix 3 and 5 butterflies.
    roots3: [Complex; 3],
    roots5: [Complex; 5],
}

impl Fft {
    fn new(n: usize) -> Self {
        let mut factors = Vec::new();
        let mut left = n;
        for radix in [4, 2, 3, 5] {
            while left % radix == 0 {
                factors.push(radix);
                left /= radix;
            }
        }
        assert_eq!(left, 1, "an FFT of {n} points needs another radix");
        let mut twiddles = Vec::new();
        let mut size = n;
        for &radix in &factors {
            let m = size / radix;
            let mut table = Vec::with_capacity(m * (radix - 1));
            for u in 0..m {
                for r in 1..radix {
                    table.push(root(r * u, size));
                }
            }
            twiddles.push(table);
            size = m;
        }
        Self {
            factors,
            twiddles,
            roots3: [root(0, 3), root(1, 3), root(2, 3)],
            roots5: [root(0, 5), root(1, 5), root(2, 5), root(3, 5), root(4, 5)],
        }
    }

    /// Transforms `input` into `out`.
    fn run(&self, input: &[Complex], out: &mut [Complex]) {
        self.stage(input, 0, 1, out, 0);
    }

    fn stage(
        &self,
        input: &[Complex],
        at: usize,
        stride: usize,
        out: &mut [Complex],
        level: usize,
    ) {
        let n = out.len();
        let radix = self.factors[level];
        let m = n / radix;
        if m == 1 {
            for (q, value) in out.iter_mut().enumerate() {
                *value = input[at + q * stride];
            }
        } else {
            for q in 0..radix {
                self.stage(
                    input,
                    at + q * stride,
                    stride * radix,
                    &mut out[q * m..(q + 1) * m],
                    level + 1,
                );
            }
        }
        // The butterflies: the sub-transforms twiddled, then a transform of
        // `radix` points across them.
        let twiddles = &self.twiddles[level];
        for u in 0..m {
            let w = &twiddles[u * (radix - 1)..(u + 1) * (radix - 1)];
            match radix {
                2 => {
                    let a0 = out[u];
                    let a1 = out[u + m].mul(w[0]);
                    out[u] = a0.add(a1);
                    out[u + m] = Complex {
                        re: a0.re - a1.re,
                        im: a0.im - a1.im,
                    };
                }
                4 => {
                    let a0 = out[u];
                    let a1 = out[u + m].mul(w[0]);
                    let a2 = out[u + 2 * m].mul(w[1]);
                    let a3 = out[u + 3 * m].mul(w[2]);
                    let s02 = a0.add(a2);
                    let d02 = Complex {
                        re: a0.re - a2.re,
                        im: a0.im - a2.im,
                    };
                    let s13 = a1.add(a3);
                    let d13 = Complex {
                        re: a1.re - a3.re,
                        im: a1.im - a3.im,
                    };
                    out[u] = s02.add(s13);
                    // -i (a1 - a3) and i (a1 - a3).
                    out[u + m] = Complex {
                        re: d02.re + d13.im,
                        im: d02.im - d13.re,
                    };
                    out[u + 2 * m] = Complex {
                        re: s02.re - s13.re,
                        im: s02.im - s13.im,
                    };
                    out[u + 3 * m] = Complex {
                        re: d02.re - d13.im,
                        im: d02.im + d13.re,
                    };
                }
                _ => {
                    let roots: &[Complex] = if radix == 3 {
                        &self.roots3
                    } else {
                        &self.roots5
                    };
                    let mut a = [Complex::default(); 5];
                    a[0] = out[u];
                    for r in 1..radix {
                        a[r] = out[u + r * m].mul(w[r - 1]);
                    }
                    for q in 0..radix {
                        let mut sum = a[0];
                        for (r, &value) in a.iter().enumerate().take(radix).skip(1) {
                            sum = sum.add(value.mul(roots[(r * q) % radix]));
                        }
                        out[u + q * m] = sum;
                    }
                }
            }
        }
    }
}

/// The tables of the transform, the same for every decoder.
struct Tables {
    /// For each `shift` from 0 (the 20 ms block) to `MAX_LM` (2.5 ms),
    /// `cos(2 pi (i + 1/8) / N)` for `i < N / 2`.
    trig: Vec<Vec<f32>>,
    ffts: Vec<Fft>,
    /// The window over the overlap: `sin(pi/2 sin^2(pi/2 (i + 1/2) / overlap))`.
    window: [f32; OVERLAP],
}

fn tables() -> &'static Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    TABLES.get_or_init(Tables::build)
}

/// The window of the overlap.
pub(crate) fn window() -> &'static [f32; OVERLAP] {
    &tables().window
}

/// The inverse MDCT for every block size CELT uses, with its work space.
pub(crate) struct Imdct {
    work: Vec<Complex>,
    folded: Vec<Complex>,
}

impl Tables {
    fn build() -> Self {
        let longest = 2 * (SHORT_MDCT << MAX_LM);
        // The reference works these out with its own single-precision pi.
        let pi = f64::from(std::f32::consts::PI);
        let mut trig = Vec::new();
        let mut ffts = Vec::new();
        for shift in 0..=MAX_LM {
            let n = longest >> shift;
            trig.push(
                (0..n / 2)
                    .map(|i| (2.0 * pi * (i as f64 + 0.125) / n as f64).cos() as f32)
                    .collect(),
            );
            ffts.push(Fft::new(n / 4));
        }
        let mut window = [0.0f32; OVERLAP];
        for (i, value) in window.iter_mut().enumerate() {
            let inner = (0.5 * PI * (i as f64 + 0.5) / OVERLAP as f64).sin();
            *value = (0.5 * PI * inner * inner).sin() as f32;
        }
        Self { trig, ffts, window }
    }
}

impl Imdct {
    pub(crate) fn new() -> Self {
        let longest = 2 * (SHORT_MDCT << MAX_LM);
        Self {
            work: vec![Complex::default(); longest / 4],
            folded: vec![Complex::default(); longest / 4],
        }
    }

    /// Transforms the `N / 2` coefficients `input[0], input[stride], ...`,
    /// where `N = 1920 >> shift`, into `out`: `N / 2` samples from
    /// `out[OVERLAP / 2]` on, the first `OVERLAP` of them overlapped with
    /// what `out[..OVERLAP / 2]` held, the tail of the block before
    /// (`clt_mdct_backward`).
    pub(crate) fn backward(&mut self, input: &[f32], stride: usize, out: &mut [f32], shift: usize) {
        let n = (2 * (SHORT_MDCT << MAX_LM)) >> shift;
        let n2 = n / 2;
        let n4 = n / 4;
        let tables = tables();
        let trig = &tables.trig[shift];

        // Fold and rotate.
        for i in 0..n4 {
            let x1 = input[2 * i * stride];
            let x2 = input[(n2 - 1 - 2 * i) * stride];
            let yr = x2 * trig[i] + x1 * trig[n4 + i];
            let yi = x1 * trig[i] - x2 * trig[n4 + i];
            // Real and imaginary swapped: a forward FFT stands in for an
            // inverse one.
            self.folded[i] = Complex { re: yi, im: yr };
        }
        tables.ffts[shift].run(&self.folded[..n4], &mut self.work[..n4]);

        // Rotate back, from both ends at once.
        let base = OVERLAP / 2;
        let y = &mut out[base..base + n2];
        for i in 0..(n4 + 1) >> 1 {
            let z0 = self.work[i];
            let z1 = self.work[n4 - 1 - i];
            let (re, im) = (z0.im, z0.re);
            let (t0, t1) = (trig[i], trig[n4 + i]);
            let yr0 = re * t0 + im * t1;
            let yi0 = re * t1 - im * t0;
            let (re, im) = (z1.im, z1.re);
            let (t0, t1) = (trig[n4 - i - 1], trig[n2 - i - 1]);
            let yr1 = re * t0 + im * t1;
            let yi1 = re * t1 - im * t0;
            y[2 * i] = yr0;
            y[n2 - 1 - 2 * i] = yi0;
            y[n2 - 2 - 2 * i] = yr1;
            y[2 * i + 1] = yi1;
        }

        // Unfold around the window, adding the overlap.
        let window = &tables.window;
        for i in 0..OVERLAP / 2 {
            let x1 = out[OVERLAP - 1 - i];
            let x2 = out[i];
            let w1 = window[i];
            let w2 = window[OVERLAP - 1 - i];
            out[i] = w2 * x2 - w1 * x1;
            out[OVERLAP - 1 - i] = w1 * x2 + w2 * x1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::needless_range_loop)]
    fn the_fft_is_the_dft() {
        for n in [60, 120, 240, 480] {
            let fft = Fft::new(n);
            let input: Vec<Complex> = (0..n)
                .map(|i| Complex {
                    re: ((i * 7 % 13) as f32 - 6.0) / 7.0,
                    im: ((i * 5 % 11) as f32 - 5.0) / 9.0,
                })
                .collect();
            let mut out = vec![Complex::default(); n];
            fft.run(&input, &mut out);
            for k in 0..n {
                let (mut re, mut im) = (0.0f64, 0.0f64);
                for (j, value) in input.iter().enumerate() {
                    let angle = -2.0 * PI * (j * k) as f64 / n as f64;
                    re += f64::from(value.re) * angle.cos() - f64::from(value.im) * angle.sin();
                    im += f64::from(value.re) * angle.sin() + f64::from(value.im) * angle.cos();
                }
                assert!((f64::from(out[k].re) - re).abs() < 1e-3, "{n} {k}");
                assert!((f64::from(out[k].im) - im).abs() < 1e-3, "{n} {k}");
            }
        }
    }

    /// Past the overlap the window is flat, and the block is the type IV
    /// discrete cosine transform `u` of the coefficients, backwards and
    /// negated: what the folding of the reference works out to.
    #[test]
    fn the_flat_part_is_a_reversed_dct_iv() {
        let mut imdct = Imdct::new();
        for (shift, stride) in [(3, 1), (0, 1), (3, 8)] {
            let n = (2 * (SHORT_MDCT << MAX_LM)) >> shift;
            let n2 = n / 2;
            let input: Vec<f32> = (0..n2 * stride)
                .map(|k| ((k * 37 % 17) as f32 - 8.0) / 8.0)
                .collect();
            let mut out = vec![0.0f32; n2 + OVERLAP];
            imdct.backward(&input, stride, &mut out, shift);
            let u = |k: usize| -> f64 {
                (0..n2)
                    .map(|j| {
                        f64::from(input[j * stride])
                            * (PI / n2 as f64 * (j as f64 + 0.5) * (k as f64 + 0.5)).cos()
                    })
                    .sum()
            };
            for j in OVERLAP / 2..n2 {
                let want = -u(n2 - 1 - j);
                let got = f64::from(out[OVERLAP / 2 + j]);
                assert!(
                    (got - want).abs() < 1e-3,
                    "{shift}, {j}: {got} against {want}"
                );
            }
        }
    }
}
