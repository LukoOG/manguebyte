//! Jacobi elliptic functions, and the elliptic (Cauer) low-pass prototype
//! built from them.
//!
//! The functions follow S. J. Orfanidis, "Lecture Notes on Elliptic Filter
//! Design" (Rutgers University, 2006): the Jacobi functions `sn` and `cd`,
//! with their argument in units of the quarter period `K`, by the descending
//! Landen transformation, which works for complex arguments too. The
//! elliptic integrals come from B. C. Carlson, "Numerical computation of
//! real or complex elliptic integrals", Numerical Algorithms 10 (1995),
//! by his duplication algorithm for `R_F`.

use core::f64::consts::{LN_10, PI};

use crate::fft::Complex;

type C64 = Complex<f64>;

/// Most pole pairs of a prototype.
const MAX_PAIRS: usize = crate::iir::MAX_ORDER / 2;

/// Carlson's symmetric elliptic integral `R_F(x, y, z)`, for `x, y, z` of
/// zero or more, at most one of them zero.
///
/// Each duplication step brings the three arguments four times closer
/// together; once they agree to within 1e-3 of their mean, the fifth-order
/// series that finishes the job is exact to well below 1e-16.
pub(crate) fn carlson_rf(mut x: f64, mut y: f64, mut z: f64) -> f64 {
    for _ in 0..64 {
        let mean = (x + y + z) / 3.0;
        let spread = (mean - x).abs().max((mean - y).abs()).max((mean - z).abs());
        if spread <= 1e-3 * mean {
            break;
        }
        let (sx, sy, sz) = (x.sqrt(), y.sqrt(), z.sqrt());
        let lambda = sx * (sy + sz) + sy * sz;
        x = (x + lambda) / 4.0;
        y = (y + lambda) / 4.0;
        z = (z + lambda) / 4.0;
    }
    let mean = (x + y + z) / 3.0;
    let (dx, dy) = (1.0 - x / mean, 1.0 - y / mean);
    let dz = -(dx + dy);
    let e2 = dx * dy - dz * dz;
    let e3 = dx * dy * dz;
    (1.0 - e2 / 10.0 + e3 / 14.0 + e2 * e2 / 24.0 - 3.0 * e2 * e3 / 44.0) / mean.sqrt()
}

/// An elliptic modulus `k`, kept together with its complement
/// `k' = sqrt(1 - k²)`, each computed so that it keeps its digits when the
/// other is close to 1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Modulus {
    pub(crate) k: f64,
    pub(crate) complement: f64,
}

impl Modulus {
    pub(crate) fn from_k(k: f64) -> Self {
        Self {
            k,
            complement: ((1.0 - k) * (1.0 + k)).sqrt(),
        }
    }

    pub(crate) fn from_complement(complement: f64) -> Self {
        Self {
            k: ((1.0 - complement) * (1.0 + complement)).sqrt(),
            complement,
        }
    }

    /// The complementary modulus.
    pub(crate) fn swapped(self) -> Self {
        Self {
            k: self.complement,
            complement: self.k,
        }
    }

    /// The quarter period `K(k)`, the complete elliptic integral of the
    /// first kind: `R_F(0, k'², 1)`.
    pub(crate) fn quarter_period(self) -> f64 {
        carlson_rf(0.0, self.complement * self.complement, 1.0)
    }

    /// The incomplete elliptic integral of the first kind `F(φ, k)`:
    /// `sin φ · R_F(cos² φ, 1 - k² sin² φ, 1)`, with `1 - k² sin² φ` taken
    /// as `cos² φ + k'² sin² φ` so that nothing cancels.
    pub(crate) fn integral(self, phi: f64) -> f64 {
        let (sin, cos) = phi.sin_cos();
        let cos2 = cos * cos;
        sin * carlson_rf(cos2, cos2 + (self.complement * sin).powi(2), 1.0)
    }
}

/// The descending Landen moduli of a modulus, `k_n = (k_{n-1} / (1 +
/// k'_{n-1}))²`, down to where the next would no longer change a result.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Landen {
    moduli: [f64; 24],
    /// `1 - k_n` for each of them, which is `2·k'_{n-1} / (1 + k'_{n-1})`:
    /// kept apart, because a modulus close to 1 cannot hold it.
    gaps: [f64; 24],
    count: usize,
}

impl Landen {
    pub(crate) fn new(modulus: Modulus) -> Self {
        let mut landen = Self {
            moduli: [0.0; 24],
            gaps: [0.0; 24],
            count: 0,
        };
        let (mut k, mut complement) = (modulus.k, modulus.complement);
        while landen.count < landen.moduli.len() && k > 1e-17 {
            // The complement of the next modulus, 2·sqrt(k')/(1 + k'), is
            // taken directly, so a modulus close to 1 loses nothing.
            let next = (k / (1.0 + complement)).powi(2);
            landen.gaps[landen.count] = 2.0 * complement / (1.0 + complement);
            complement = 2.0 * complement.sqrt() / (1.0 + complement);
            k = next;
            landen.moduli[landen.count] = k;
            landen.count += 1;
        }
        landen
    }

    /// `1 - cd(u·K, k)` for a real `u`, computed without cancellation where
    /// `cd` is close to 1.
    ///
    /// It follows the same ascent as [`cd`](Self::cd): with `e = 1 - w`,
    /// the step `w ← (1 + v)·w / (1 + v·w²)` is `e ← e·((1 - v) + v·e) /
    /// (1 + v·w²)`, which only multiplies small numbers known to full
    /// precision, starting from `1 - cos(u·π/2) = 2·sin²(u·π/4)`.
    pub(crate) fn cd_complement(&self, u: f64) -> f64 {
        let mut w = (u * PI / 2.0).cos();
        let mut e = 2.0 * (u * PI / 4.0).sin().powi(2);
        for (&v, &gap) in self.moduli[..self.count]
            .iter()
            .zip(&self.gaps[..self.count])
            .rev()
        {
            let below = 1.0 + v * w * w;
            e *= (gap + v * e) / below;
            w *= (1.0 + v) / below;
        }
        e
    }

    /// `cd(u·K, k)`, for `u` in units of the quarter period.
    pub(crate) fn cd(&self, u: C64) -> C64 {
        self.ascend(cos(u.scale(PI / 2.0)))
    }

    /// `sn(u·K, k)`, for `u` in units of the quarter period.
    pub(crate) fn sn(&self, u: C64) -> C64 {
        self.ascend(sin(u.scale(PI / 2.0)))
    }

    /// From the function at modulus 0, where `cd` is a cosine and `sn` a
    /// sine, back up the Landen moduli: `w ← (1 + v)·w / (1 + v·w²)`.
    fn ascend(&self, mut w: C64) -> C64 {
        let one = C64::new(1.0, 0.0);
        for &v in self.moduli[..self.count].iter().rev() {
            w = divide(w.scale(1.0 + v), one + (w * w).scale(v));
        }
        w
    }
}

fn divide(a: C64, b: C64) -> C64 {
    let size = b.norm_sqr();
    C64::new(
        (a.re * b.re + a.im * b.im) / size,
        (a.im * b.re - a.re * b.im) / size,
    )
}

fn cos(z: C64) -> C64 {
    C64::new(z.re.cos() * z.im.cosh(), -z.re.sin() * z.im.sinh())
}

fn sin(z: C64) -> C64 {
    C64::new(z.re.sin() * z.im.cosh(), z.re.cos() * z.im.sinh())
}

/// `(θ2(q) / θ3(q))²`: the modulus whose nome is `q`, for `q` up to
/// `e^-π`, where eight terms of each series reach the last digit.
fn modulus_of_nome(q: f64) -> f64 {
    let (mut two, mut three) = (0.0, 0.0);
    for m in 0..8 {
        two += q.powi(m * (m + 1));
        if m > 0 {
            three += q.powi(m * m);
        }
    }
    let ratio = 2.0 * q.powf(0.25) * two / (1.0 + 2.0 * three);
    ratio * ratio
}

/// The selectivity `k` of an elliptic filter of order `n` with
/// discrimination `k1`: the solution of the degree equation
/// `K'(k) / K(k) = K'(k1) / (n·K(k1))`.
///
/// The nome `q = e^(-π·K'/K)` of the solution is the `n`-th root of the
/// nome of `k1`. When it is small, `k` comes straight from it; when it is
/// not, the complementary nome `e^(-π·K/K')` is, and gives `k'` instead.
pub(crate) fn selectivity(order: usize, discrimination: Modulus) -> Modulus {
    let ratio = discrimination.swapped().quarter_period()
        / (order as f64 * discrimination.quarter_period());
    if ratio >= 1.0 {
        Modulus::from_k(modulus_of_nome((-PI * ratio).exp()))
    } else {
        Modulus::from_complement(modulus_of_nome((-PI / ratio).exp()))
    }
}

/// The elliptic low-pass prototype, with its pass-band edge at 1 rad/s:
/// ripple `εp` in the pass band, the stop band from `1/k` up, and
/// `|H|² = 1 / (1 + εp²·R_N(ω)²)` with `R_N` the elliptic rational
/// function.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Elliptic {
    pub(crate) order: usize,
    /// `εp²`.
    pub(crate) ripple: f64,
    /// The selectivity: pass-band edge over stop-band edge.
    pub(crate) selectivity: Modulus,
    /// `ζ_i = cd(u_i·K, k)` for `u_i = (2i - 1) / N`: where `R_N` is zero.
    /// The zeros of the filter are at `±j / (k·ζ_i)`.
    pub(crate) zeros: [f64; MAX_PAIRS],
    /// `1 - ζ_i`, which a `ζ_i` close to 1 cannot hold to full precision.
    gaps: [f64; MAX_PAIRS],
    /// The poles above the real axis, in the same order.
    pub(crate) poles: [C64; MAX_PAIRS],
    /// The real pole of an odd order.
    pub(crate) real: Option<f64>,
    /// How many pole pairs there are.
    pub(crate) count: usize,
}

impl Elliptic {
    /// The prototype of order `order` with `ripple_db` of ripple in the
    /// pass band and the stop band `attenuation_db` down, which must be
    /// more than the ripple.
    pub(crate) fn new(order: usize, ripple_db: f64, attenuation_db: f64) -> Self {
        let n = order as f64;
        let ripple = (ripple_db * LN_10 / 10.0).exp_m1();
        let stop = (attenuation_db * LN_10 / 10.0).exp_m1();
        let discrimination = Modulus::from_k((ripple / stop).sqrt());
        let selectivity = selectivity(order, discrimination);
        let landen = Landen::new(selectivity);
        // The poles lie off the zeros of R_N by -j·v0, where
        // sn(j·v0·N·K1, k1) = j / εp, that is
        // v0 = F(atan(1 / εp), k1') / (N·K1).
        let v0 = discrimination
            .swapped()
            .integral((1.0 / ripple.sqrt()).atan())
            / (n * discrimination.quarter_period());
        let mut prototype = Self {
            order,
            ripple,
            selectivity,
            zeros: [0.0; MAX_PAIRS],
            gaps: [0.0; MAX_PAIRS],
            poles: [C64::ZERO; MAX_PAIRS],
            real: None,
            count: order / 2,
        };
        for i in 0..order / 2 {
            let u = (2 * i + 1) as f64 / n;
            prototype.zeros[i] = landen.cd(C64::new(u, 0.0)).re;
            prototype.gaps[i] = landen.cd_complement(u);
            // j·cd((u - j·v0)·K, k).
            let w = landen.cd(C64::new(u, -v0));
            prototype.poles[i] = C64::new(-w.im, w.re);
        }
        if order % 2 == 1 {
            // j·sn(j·v0·K, k), which is real.
            prototype.real = Some(-landen.sn(C64::new(0.0, v0)).im);
        }
        prototype
    }

    /// The gain at 0 rad/s: 1 for an odd order, the bottom of the ripple
    /// for an even one.
    pub(crate) fn gain(&self) -> f64 {
        if self.order % 2 == 1 {
            1.0
        } else {
            1.0 / (1.0 + self.ripple).sqrt()
        }
    }

    /// The elliptic rational function `R_N(x)`, from its zeros and poles:
    /// `x^(N mod 2) · Π (x² - ζ_i²) / (1 - k²·ζ_i²·x²)`, scaled to be 1 at
    /// `x = 1`.
    ///
    /// For a selectivity `k` close to 1, `1 - k²·ζ²·x²` and `1 - ζ²` are
    /// small differences of numbers near 1, which written that way lose
    /// their digits, all of them once `k` rounds to 1. They are taken
    /// instead as `(1 - ζ·x)(1 + ζ·x) + k'²·ζ²·x²`, with `1 - ζ·x = (1 - x)
    /// + (1 - ζ)·x`, and `(1 - ζ)(1 + ζ)`, from the complement `k'` and
    /// `1 - ζ`, which are both kept to full precision; and `x² - ζ²` as
    /// `(x - 1)(x + 1) + (1 - ζ²)`.
    pub(crate) fn rational(&self, x: f64) -> f64 {
        let k = self.selectivity.k;
        let complement2 = self.selectivity.complement.powi(2);
        let odd = self.order % 2 == 1;
        let pairs = self.zeros[..self.count].iter().zip(&self.gaps);
        let mut at_one = 1.0;
        for (&zeta, &gap) in pairs.clone() {
            let below = gap * (2.0 - gap);
            at_one *= below / (below + complement2 * zeta * zeta);
        }
        if x.abs() > 1e150 {
            // Each factor tends to -1 / (k·ζ_i)².
            if odd {
                return f64::INFINITY.copysign(x);
            }
            let mut value = 1.0;
            for &zeta in &self.zeros[..self.count] {
                value *= -1.0 / (k * zeta).powi(2);
            }
            return value / at_one;
        }
        let mut value = if odd { x } else { 1.0 };
        for (&zeta, &gap) in pairs {
            let below = gap * (2.0 - gap);
            let numerator = (x - 1.0) * (x + 1.0) + below;
            let denominator =
                ((1.0 - x) + gap * x) * (1.0 + zeta * x) + complement2 * (zeta * x).powi(2);
            value *= numerator / denominator;
        }
        value / at_one
    }

    /// The smallest damping ratio of a pole, `-Re(p) / |p|`: 1 for a real
    /// pole, and close to 0 for a pole close to the imaginary axis.
    pub(crate) fn smallest_damping(&self) -> f64 {
        self.poles[..self.count]
            .iter()
            .map(|pole| -pole.re / pole.norm())
            .fold(1.0, f64::min)
    }
}

/// The least damping ratio a pole of an elliptic prototype is allowed.
///
/// Below it a pole is so close to the imaginary axis that double precision
/// cannot place it: computed to an absolute 1e-16, its real part would be
/// uncertain by 1e-8 of itself at this damping, and by all of itself near
/// 1e-16, where the digital sections it makes round onto the unit circle.
/// A filter with such a pole rings for hours. Specifications that ask for
/// one are those with very little room between the ripple and the
/// attenuation at a high order (3 dB and 4 dB at order 11, say), whose
/// transition band is narrower than anything can be measured.
pub(crate) const LEAST_DAMPING: f64 = 1e-8;

/// The attenuation of an elliptic design of `order` with `ripple_db`:
/// `attenuation_db`, or, if that leaves a pole with less damping than
/// [`LEAST_DAMPING`], the least attenuation up to `most_db` that does not
/// (to within 1e-6 dB above it).
///
/// More attenuation widens the transition band, which moves the poles away
/// from the imaginary axis, so the least such attenuation is found by
/// bisection. It costs nothing more than one prototype for a design that
/// is not limited, and about thirty for one that is.
pub(crate) fn least_attenuation(
    order: usize,
    ripple_db: f64,
    attenuation_db: f64,
    most_db: f64,
) -> f64 {
    let damped = |db: f64| Elliptic::new(order, ripple_db, db).smallest_damping() >= LEAST_DAMPING;
    if attenuation_db >= most_db || damped(attenuation_db) {
        return attenuation_db;
    }
    let (mut low, mut high) = (attenuation_db, most_db);
    while high - low > 1e-6 {
        let middle = 0.5 * (low + high);
        if middle <= low || middle >= high {
            break;
        }
        if damped(middle) {
            high = middle;
        } else {
            low = middle;
        }
    }
    high
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_integrals_have_their_known_values() {
        // K(0) = π/2, and K(1/√2) = Γ(1/4)² / (4·√π).
        let k0 = Modulus::from_k(0.0).quarter_period();
        assert!((k0 - PI / 2.0).abs() < 1e-15);
        let gamma_quarter = 3.625_609_908_221_908;
        let lemniscate = gamma_quarter * gamma_quarter / (4.0 * PI.sqrt());
        let half = Modulus::from_k(core::f64::consts::FRAC_1_SQRT_2);
        assert!((half.quarter_period() / lemniscate - 1.0).abs() < 1e-14);
        // F(π/2, k) = K(k), and F(φ, 0) = φ.
        for k in [0.1, 0.5, 0.9, 0.999_999] {
            let modulus = Modulus::from_k(k);
            let ratio = modulus.integral(PI / 2.0) / modulus.quarter_period();
            assert!((ratio - 1.0).abs() < 1e-14);
        }
        assert!((Modulus::from_k(0.0).integral(0.7) - 0.7).abs() < 1e-15);
        // Near k = 1, K(k) = ln(4 / k') to within k'² ln(k').
        let close = Modulus::from_complement(1e-9);
        assert!((close.quarter_period() - (4e9f64).ln()).abs() < 1e-12);
    }

    #[test]
    fn landen_functions_agree_with_carlson_integrals() {
        // sn(F(φ, k), k) = sin φ, cd(0) = 1, cd(K) = 0, sn(K) = 1, and
        // sn(K/2) = cd(K/2) = 1 / sqrt(1 + k').
        for k in [0.0, 1e-9, 0.3, 0.7, 0.99, 1.0 - 1e-12] {
            let modulus = Modulus::from_k(k);
            let landen = Landen::new(modulus);
            let quarter = modulus.quarter_period();
            for phi in [0.1, 0.5, 1.0, 1.4] {
                let u = modulus.integral(phi) / quarter;
                let sn = landen.sn(C64::new(u, 0.0));
                assert!(
                    (sn.re - phi.sin()).abs() < 1e-13 && sn.im == 0.0,
                    "k {k}, φ {phi}"
                );
            }
            assert!((landen.cd(C64::ZERO).re - 1.0).abs() < 1e-15);
            assert!(landen.cd(C64::new(1.0, 0.0)).re.abs() < 1e-14);
            assert!((landen.sn(C64::new(1.0, 0.0)).re - 1.0).abs() < 1e-15);
            let half = 1.0 / (1.0 + modulus.complement).sqrt();
            assert!((landen.sn(C64::new(0.5, 0.0)).re - half).abs() < 1e-14);
            assert!((landen.cd(C64::new(0.5, 0.0)).re - half).abs() < 1e-14);
        }
        // An imaginary argument: sn(j·F(φ, k')) = j·tan φ, by Jacobi's
        // imaginary transformation.
        for k in [0.2, 0.6, 0.95] {
            let modulus = Modulus::from_k(k);
            let landen = Landen::new(modulus);
            for phi in [0.2, 0.8, 1.3] {
                let v = modulus.swapped().integral(phi) / modulus.quarter_period();
                let sn = landen.sn(C64::new(0.0, v));
                assert!(sn.re.abs() < 1e-14, "{sn:?}");
                assert!((sn.im / phi.tan() - 1.0).abs() < 1e-13, "{sn:?}");
            }
        }
    }

    #[test]
    fn the_selectivity_solves_the_degree_equation() {
        // K'(k)/K(k) against K'(k1)/(N·K(k1)), each side from Carlson's
        // integrals, for small and large solutions alike.
        for (ripple_db, attenuation_db) in [(0.001, 200.0), (0.1, 60.0), (1.0, 40.0), (3.0, 20.0)] {
            let ripple = (ripple_db * LN_10 / 10.0_f64).exp_m1();
            let stop = (attenuation_db * LN_10 / 10.0_f64).exp_m1();
            let discrimination = Modulus::from_k((ripple / stop).sqrt());
            for order in 1..=16 {
                let k = selectivity(order, discrimination);
                assert!(k.k > 0.0 && k.k < 1.0 && k.complement > 0.0);
                let left = k.swapped().quarter_period() / k.quarter_period();
                let right = discrimination.swapped().quarter_period()
                    / (order as f64 * discrimination.quarter_period());
                assert!(
                    (left / right - 1.0).abs() < 1e-12,
                    "{order}: {left} against {right}"
                );
                // And the two halves of the modulus belong together.
                let norm = k.k.hypot(k.complement);
                assert!((norm - 1.0).abs() < 1e-15);
            }
            // Order 1 is the trivial solution k = k1.
            let k = selectivity(1, discrimination);
            assert!((k.k / discrimination.k - 1.0).abs() < 1e-12);
        }
    }

    #[test]
    fn the_rational_function_ripples_between_minus_one_and_one() {
        for order in 1..=16 {
            let prototype = Elliptic::new(order, 0.5, 60.0);
            let landen = Landen::new(prototype.selectivity);
            // R_N(cd(m·K/N)) is ±1 for even m and 0 for odd m.
            for m in 0..=order {
                let x = landen.cd(C64::new(m as f64 / order as f64, 0.0)).re;
                let value = prototype.rational(x);
                if m % 2 == 0 {
                    assert!((value.abs() - 1.0).abs() < 1e-9, "{order}, {m}: {value}");
                } else {
                    assert!(value.abs() < 1e-9, "{order}, {m}: {value}");
                }
            }
            // And in the stop band, R_N(1 / (k·x)) = 1 / (k1·R_N(x)): it
            // never comes below 1/k1 in size.
            let k1 = (prototype.ripple / (60.0 * LN_10 / 10.0_f64).exp_m1()).sqrt();
            for i in 0..=200 {
                let x = i as f64 / 200.0;
                let inside = prototype.rational(x);
                let outside = prototype.rational(1.0 / (prototype.selectivity.k * x));
                assert!(inside.abs() <= 1.0 + 1e-9);
                if inside.abs() > 1e-6 {
                    assert!((outside * inside * k1 - 1.0).abs() < 1e-7, "{order} at {x}");
                }
            }
        }
    }

    #[test]
    fn the_complement_of_cd_keeps_its_digits() {
        // Where cd is not close to 1, the plain difference is exact enough
        // to compare with.
        for k in [0.0, 0.3, 0.9, 0.999] {
            let landen = Landen::new(Modulus::from_k(k));
            for u in [0.05, 0.3, 0.5, 0.8, 1.0] {
                let plain = 1.0 - landen.cd(C64::new(u, 0.0)).re;
                let careful = landen.cd_complement(u);
                assert!((careful - plain).abs() < 1e-15, "{k}, {u}");
            }
        }
        // cd(K/2) is 1 / sqrt(1 + k') for any modulus, so its complement is
        // 1 - (1 + k')^(-1/2), about k'/2: computed here without
        // cancellation, for moduli close to 1, and so close that k rounds
        // to it.
        for complement in [1e-5, 1e-10, 1e-20] {
            let modulus = Modulus::from_complement(complement);
            assert_eq!(modulus.k == 1.0, complement < 1e-8);
            let landen = Landen::new(modulus);
            let exact = -(-0.5 * complement.ln_1p()).exp_m1();
            let careful = landen.cd_complement(0.5);
            assert!(
                (careful / exact - 1.0).abs() < 1e-12,
                "{complement}: {careful}"
            );
            // Where the plain difference has nothing left.
            if complement < 1e-16 {
                assert_eq!(1.0 - landen.cd(C64::new(0.5, 0.0)).re, 0.0);
            }
        }
    }

    /// The rational function written the plain way, as it was before:
    /// `x^(N mod 2) · Π (x² - ζ²) / (1 - k²·ζ²·x²)`, over its value at 1.
    fn plain_rational(prototype: &Elliptic, x: f64) -> f64 {
        let k = prototype.selectivity.k;
        let zeros = &prototype.zeros[..prototype.count];
        let at_one: f64 = zeros
            .iter()
            .map(|&zeta| (1.0 - zeta * zeta) / (1.0 - (k * zeta).powi(2)))
            .product();
        let odd = if prototype.order % 2 == 1 { x } else { 1.0 };
        let value: f64 = zeros
            .iter()
            .map(|&zeta| (x * x - zeta * zeta) / (1.0 - (k * zeta * x).powi(2)))
            .product();
        odd * value / at_one
    }

    #[test]
    fn the_rational_function_keeps_its_digits_when_the_selectivity_is_close_to_one() {
        // Specifications with 1 dB between ripple and attenuation at high
        // orders, before any limit on their damping: their selectivity is
        // so close to 1 that it rounds to it. R_N must still lie between -1
        // and 1 in the pass band, be ±1 at its edge and above 1/k1 in size
        // beyond the stop-band edge. The plain formula fails on the four
        // the review found: above 1 in size, or not a number.
        for (order, ripple_db, attenuation_db, plain_failed) in [
            (11, 3.0, 4.0, true),
            (12, 3.0, 4.0, true),
            (13, 1.0, 2.0, false),
            (15, 3.0, 4.0, true),
            (16, 1.0, 2.0, true),
        ] {
            let prototype = Elliptic::new(order, ripple_db, attenuation_db);
            let stop = (attenuation_db * LN_10 / 10.0_f64).exp_m1();
            let discrimination = (prototype.ripple / stop).sqrt();
            let mut plain_fails = false;
            for i in 0..=400 {
                let x = f64::from(i) / 400.0;
                let value = prototype.rational(x);
                assert!(
                    value.is_finite() && value.abs() <= 1.0 + 1e-9,
                    "{order} at {x}: {value}"
                );
                let plain = plain_rational(&prototype, x);
                plain_fails |= plain.is_nan() || plain.abs() > 1.0 + 1e-6;
            }
            assert!((prototype.rational(1.0).abs() - 1.0).abs() < 1e-12);
            for x in [1.0 + 1e-9, 1.01, 2.0, 1e3] {
                let x = x / prototype.selectivity.k;
                let value = prototype.rational(x);
                assert!(
                    value.abs() >= (1.0 - 1e-9) / discrimination,
                    "{order} at {x}: {value}"
                );
            }
            assert!(
                !plain_failed || plain_fails,
                "{order}, {ripple_db}, {attenuation_db}"
            );
        }
    }

    #[test]
    fn the_least_attenuation_gives_the_least_damping_and_no_more() {
        // Elliptic specifications that leave a pole damped by less than
        // 1e-8 get just enough more attenuation to reach it; the module
        // documentation quotes these.
        for (order, ripple_db, expected) in [
            (11, 3.0, 11.11),
            (16, 0.1, 6.67),
            (16, 1.0, 16.17),
            (16, 3.0, 22.58),
        ] {
            let attenuation = least_attenuation(order, ripple_db, ripple_db + 1.0, 200.0);
            assert!(
                (attenuation - expected).abs() < 0.005,
                "{order}, {ripple_db}: {attenuation}"
            );
            let damping = |db: f64| Elliptic::new(order, ripple_db, db).smallest_damping();
            assert!(damping(attenuation) >= LEAST_DAMPING);
            assert!(damping(attenuation - 1e-5) < LEAST_DAMPING);
        }
        // Specifications with room to spare are left as they are.
        for order in 1..=16 {
            for (ripple_db, attenuation_db) in [(0.5, 60.0), (3.0, 25.0), (0.001, 1.001)] {
                let attenuation = least_attenuation(order, ripple_db, attenuation_db, 200.0);
                assert_eq!(attenuation, attenuation_db);
            }
        }
        // The damping falls as the attenuation comes closer to the ripple,
        // which is what makes the bisection right.
        let mut before = 0.0;
        for step in 0..=40 {
            let damping = Elliptic::new(16, 1.0, 2.0 + f64::from(step)).smallest_damping();
            assert!(damping > before);
            before = damping;
        }
    }
}
