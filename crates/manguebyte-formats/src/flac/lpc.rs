//! Linear prediction: finding coefficients that predict each sample from
//! the ones before it.
//!
//! The method is the autocorrelation method with the Levinson-Durbin
//! recursion, as described by J. Makhoul, "Linear prediction: a tutorial
//! review", Proceedings of the IEEE 63(4), 1975. The block is multiplied by
//! a window first, so that its ends, which the method treats as if the
//! signal were zero beyond them, do not distort the estimate. The
//! coefficients come out as real numbers; FLAC stores integers and a shift,
//! so they are then quantised.

/// The highest order FLAC can store.
pub(super) const MAX_ORDER: usize = 32;

/// A window over part of a block: the `part`-th of `parts` equal stretches,
/// with zero outside it.
///
/// A window over the whole block suits audio that does not change within
/// it. One over a part fits the predictor to that part alone, which wins
/// when a block holds a note's attack and then its sustain; the encoder
/// tries both at its higher levels and keeps what codes smaller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Window {
    pub(super) part: u8,
    pub(super) parts: u8,
}

impl Window {
    pub(super) const WHOLE: Self = Self { part: 0, parts: 1 };

    /// The stretch of a block of `len` samples this window covers.
    fn span(self, len: usize) -> (usize, usize) {
        let parts = usize::from(self.parts.max(1));
        let part = usize::from(self.part).min(parts - 1);
        (len * part / parts, len * (part + 1) / parts)
    }

    /// Fills `out` with the window for a block of `out.len()` samples: a
    /// Tukey window of ratio 1/2 over its stretch, zero elsewhere.
    ///
    /// The Tukey window is flat in the middle and falls to zero over its
    /// outer quarters along half a cosine period (F. J. Harris, "On the use
    /// of windows for harmonic analysis with the discrete Fourier
    /// transform", Proceedings of the IEEE 66(1), 1978).
    pub(super) fn fill(self, out: &mut [f64]) {
        let (start, end) = self.span(out.len());
        out.fill(0.0);
        let len = end - start;
        if len == 1 {
            out[start] = 1.0;
        }
        if len < 2 {
            return;
        }
        const RATIO: f64 = 0.5;
        let last = (len - 1) as f64;
        for (index, value) in out[start..end].iter_mut().enumerate() {
            let x = index as f64 / last;
            let edge = x.min(1.0 - x);
            *value = if edge < RATIO / 2.0 {
                0.5 * (1.0 - (2.0 * std::f64::consts::PI * edge / RATIO).cos())
            } else {
                1.0
            };
        }
    }

    /// The stretch of a block of `len` samples where the window is not
    /// zero.
    pub(super) fn support(self, len: usize) -> (usize, usize) {
        self.span(len)
    }
}

/// Computes the autocorrelation of `signal` seen through `window`, for lags
/// `0..=lags`, into `out`. `windowed` is scratch space as long as the
/// signal.
pub(super) fn autocorrelation(
    signal: &[i64],
    window: &[f64],
    (start, end): (usize, usize),
    windowed: &mut [f64],
    lags: usize,
    out: &mut [f64; MAX_ORDER + 1],
) {
    let windowed = &mut windowed[start..end];
    for ((value, &sample), &weight) in windowed
        .iter_mut()
        .zip(&signal[start..end])
        .zip(&window[start..end])
    {
        *value = sample as f64 * weight;
    }
    for (lag, result) in out.iter_mut().enumerate().take(lags + 1) {
        *result = if lag < windowed.len() {
            windowed[lag..]
                .iter()
                .zip(windowed.iter())
                .map(|(a, b)| a * b)
                .sum()
        } else {
            0.0
        };
    }
}

/// The predictors of every order up to some maximum, from one
/// autocorrelation.
pub(super) struct Predictors {
    /// `coefficients[m - 1][..m]` predicts with order `m`: the first
    /// coefficient weighs the sample just before, the next the one before
    /// that, and so on.
    pub(super) coefficients: [[f64; MAX_ORDER]; MAX_ORDER],
    /// `error[m]` is what is left of the windowed signal's energy after
    /// prediction with order `m`; `error[0]` is the energy itself.
    pub(super) error: [f64; MAX_ORDER + 1],
    /// The highest order the recursion reached. It stops early when the
    /// signal is fully predicted or the arithmetic stops being reliable.
    pub(super) max_order: usize,
}

impl Predictors {
    pub(super) fn new() -> Self {
        Self {
            coefficients: [[0.0; MAX_ORDER]; MAX_ORDER],
            error: [0.0; MAX_ORDER + 1],
            max_order: 0,
        }
    }

    /// Solves for the predictors of orders `1..=max_order` with the
    /// Levinson-Durbin recursion (Makhoul 1975, equations 38a to 38e).
    pub(super) fn solve(&mut self, autocorrelation: &[f64; MAX_ORDER + 1], max_order: usize) {
        let max_order = max_order.min(MAX_ORDER);
        self.max_order = 0;
        let mut error = autocorrelation[0];
        self.error[0] = error;
        if !(error > 0.0 && error.is_finite()) {
            return;
        }
        let mut current = [0.0f64; MAX_ORDER];
        for order in 0..max_order {
            // The reflection coefficient: how much of what the current
            // predictor leaves unexplained one lag further out it can take.
            let mut numerator = autocorrelation[order + 1];
            for (coefficient, &value) in current[..order]
                .iter()
                .zip(autocorrelation[1..=order].iter().rev())
            {
                numerator -= coefficient * value;
            }
            let reflection = numerator / error;
            if !reflection.is_finite() || reflection.abs() >= 1.0 {
                return;
            }
            let previous = current;
            current[order] = reflection;
            for index in 0..order {
                current[index] = previous[index] - reflection * previous[order - 1 - index];
            }
            error *= 1.0 - reflection * reflection;
            self.coefficients[order] = current;
            self.error[order + 1] = error;
            self.max_order = order + 1;
            // Finite, since the reflection was below 1 in magnitude.
            if error <= 0.0 {
                // The signal is predicted exactly; higher orders add nothing.
                return;
            }
        }
    }
}

/// Integer coefficients for a predictor, ready to be stored.
#[derive(Debug, Clone, Copy)]
pub(super) struct Quantized {
    pub(super) coefficients: [i32; MAX_ORDER],
    pub(super) order: usize,
    /// Bits each coefficient is stored in, sign included: 1 to 15.
    pub(super) precision: u32,
    /// The prediction is the weighted sum shifted right by this: 0 to 15.
    pub(super) shift: u32,
}

/// Rounds `coefficients` to integers of at most `precision` bits, scaled by
/// a power of two chosen so that the largest uses the bits it is given.
///
/// The rounding error of each coefficient is carried into the next, so
/// that the errors do not all lean the same way. Returns `None` when every
/// coefficient rounds to zero, which predicts nothing.
pub(super) fn quantize(coefficients: &[f64], precision: u32) -> Option<Quantized> {
    let precision = precision.clamp(2, 15);
    let largest = coefficients
        .iter()
        .fold(0.0f64, |most, c| most.max(c.abs()));
    if !(largest > 0.0 && largest.is_finite()) {
        return None;
    }
    // The largest coefficient times 2^shift stays below 2^(precision - 1).
    let magnitude = largest.log2().floor() as i32;
    let shift = (precision as i32 - 2 - magnitude).clamp(0, 15);
    let scale = f64::from(1u32 << shift);
    let limit = f64::from(1u32 << (precision - 1));

    let mut out = Quantized {
        coefficients: [0; MAX_ORDER],
        order: coefficients.len(),
        precision: 1,
        shift: shift as u32,
    };
    let mut carried = 0.0;
    for (stored, &coefficient) in out.coefficients.iter_mut().zip(coefficients) {
        let wanted = coefficient * scale + carried;
        let rounded = wanted.round();
        // Only the rounding is carried. What is cut off a coefficient too
        // large for its bits is lost: no later coefficient could take it.
        carried = wanted - rounded;
        *stored = rounded.clamp(-limit, limit - 1.0) as i32;
    }
    let stored = &out.coefficients[..out.order];
    if stored.iter().all(|&value| value == 0) {
        return None;
    }
    // Store in no more bits than the values need.
    out.precision = stored
        .iter()
        .map(|&value| {
            let magnitude = if value < 0 { !value } else { value };
            33 - magnitude.leading_zeros()
        })
        .max()
        .unwrap_or(1);
    Some(out)
}

/// Computes the residual of `signal` under the predictor `predictor` into
/// `out`, which receives `signal.len() - order` values. Returns `false`,
/// leaving `out` unspecified, if a residual reaches the limit the format
/// sets.
pub(super) fn residual(signal: &[i64], predictor: &Quantized, limit: i64, out: &mut [i64]) -> bool {
    let order = predictor.order;
    let coefficients = &predictor.coefficients[..order];
    let shift = predictor.shift;
    let mut fits = true;
    for (value, (history, &sample)) in out
        .iter_mut()
        .zip(signal.windows(order).zip(&signal[order..]))
    {
        // The most recent sample is the last of the window and meets the
        // first coefficient. 33-bit samples and 15-bit coefficients give
        // products of 48 bits; 32 of them add up to at most 53.
        let prediction: i64 = coefficients
            .iter()
            .zip(history.iter().rev())
            .map(|(&coefficient, &past)| i64::from(coefficient) * past)
            .sum();
        let difference = sample - (prediction >> shift);
        fits &= difference > -limit && difference < limit;
        *value = difference;
    }
    fits
}

/// Computes the residual of `signal` under the fixed predictor of `order`
/// (RFC 9639, section 9.2.5) into `out`. Returns `false` if a residual
/// reaches the format's limit.
pub(super) fn fixed_residual(signal: &[i64], order: usize, limit: i64, out: &mut [i64]) -> bool {
    let mut fits = true;
    let mut check = |value: i64| {
        fits &= value > -limit && value < limit;
        value
    };
    let out = &mut out[..signal.len() - order];
    match order {
        0 => {
            for (value, &sample) in out.iter_mut().zip(signal) {
                *value = check(sample);
            }
        }
        1 => {
            for (value, pair) in out.iter_mut().zip(signal.windows(2)) {
                *value = check(pair[1] - pair[0]);
            }
        }
        2 => {
            for (value, s) in out.iter_mut().zip(signal.windows(3)) {
                *value = check(s[2] - 2 * s[1] + s[0]);
            }
        }
        3 => {
            for (value, s) in out.iter_mut().zip(signal.windows(4)) {
                *value = check(s[3] - 3 * s[2] + 3 * s[1] - s[0]);
            }
        }
        _ => {
            for (value, s) in out.iter_mut().zip(signal.windows(5)) {
                *value = check(s[4] - 4 * s[3] + 6 * s[2] - 4 * s[1] + s[0]);
            }
        }
    }
    fits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_whole_window_is_a_tukey_window() {
        let mut window = [0.0; 9];
        Window::WHOLE.fill(&mut window);
        // A quarter of the length tapers at each end.
        let expected = [0.0, 0.5, 1.0, 1.0, 1.0, 1.0, 1.0, 0.5, 0.0];
        for (got, want) in window.iter().zip(expected) {
            assert!((got - want).abs() < 1e-12, "{window:?}");
        }
        // Half a window covers half the block and is zero elsewhere.
        let mut window = [9.0; 18];
        Window { part: 1, parts: 2 }.fill(&mut window);
        assert!(window[..9].iter().all(|&value| value == 0.0));
        assert_eq!(window[13], 1.0);
        assert_eq!(Window { part: 1, parts: 2 }.support(18), (9, 18));
    }

    #[test]
    fn levinson_recovers_a_known_autoregressive_process() {
        // x[n] = 1.6 x[n-1] - 0.8 x[n-2] + e[n], driven by noise. The
        // estimated order-2 predictor approaches (1.6, -0.8), and higher
        // orders explain little more.
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        let mut signal = vec![0i64; 20_000];
        let (mut a, mut b) = (0.0f64, 0.0f64);
        for value in signal.iter_mut() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let noise = (state % 2001) as f64 - 1000.0;
            let next = 1.6 * a - 0.8 * b + noise;
            b = a;
            a = next;
            *value = next.round() as i64;
        }
        let mut window = vec![0.0; signal.len()];
        Window::WHOLE.fill(&mut window);
        let mut scratch = vec![0.0; signal.len()];
        let mut correlation = [0.0; MAX_ORDER + 1];
        let span = Window::WHOLE.support(signal.len());
        autocorrelation(&signal, &window, span, &mut scratch, 8, &mut correlation);
        let mut predictors = Predictors::new();
        predictors.solve(&correlation, 8);
        assert_eq!(predictors.max_order, 8);
        let order2 = &predictors.coefficients[1][..2];
        assert!((order2[0] - 1.6).abs() < 0.02, "{order2:?}");
        assert!((order2[1] + 0.8).abs() < 0.02, "{order2:?}");
        assert!(predictors.error[2] < predictors.error[1] * 0.5);
        assert!(predictors.error[8] > predictors.error[2] * 0.99);
    }

    #[test]
    fn quantised_coefficients_use_the_precision_they_are_given() {
        let coefficients = [1.6, -0.8, 0.05];
        for precision in 4..=15 {
            let quantized = quantize(&coefficients, precision).unwrap();
            assert!(quantized.precision <= precision);
            let limit = 1i32 << (quantized.precision - 1);
            let scale = f64::from(1u32 << quantized.shift);
            for (&stored, &wanted) in quantized.coefficients.iter().zip(&coefficients) {
                assert!((-limit..limit).contains(&stored));
                // Within the rounding error, which the carry may double.
                assert!(
                    (f64::from(stored) / scale - wanted).abs() <= 1.0 / scale,
                    "{precision}"
                );
            }
            // The largest coefficient uses the top bit.
            assert_eq!(quantized.precision, precision, "{quantized:?}");
        }
        // Huge coefficients are cut to fit, never wrapped.
        let quantized = quantize(&[1e9, -1e9], 15).unwrap();
        assert_eq!(quantized.shift, 0);
        assert_eq!(&quantized.coefficients[..2], &[16_383, -16_384]);
        assert!(quantize(&[0.0, 0.0], 12).is_none());
        assert!(quantize(&[1e-9], 12).is_none());
    }

    #[test]
    fn fixed_predictors_leave_nothing_of_polynomials_of_their_degree() {
        let cubic: Vec<i64> = (0..40).map(|n: i64| n * n * n - 7 * n * n + 3).collect();
        let mut out = [0i64; 40];
        for order in 0..=4 {
            assert!(fixed_residual(&cubic, order, 1 << 31, &mut out));
            let residual = &out[..cubic.len() - order];
            assert_eq!(
                residual.iter().all(|&value| value == 0),
                order == 4,
                "order {order}"
            );
        }
        // A step of 2^31 cannot be stored.
        assert!(!fixed_residual(&[0, 1 << 31], 1, 1 << 31, &mut out));
        assert!(fixed_residual(&[0, (1 << 31) - 1], 1, 1 << 31, &mut out));
    }
}
