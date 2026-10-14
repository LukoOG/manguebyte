//! The A and C frequency weightings of sound level meters.

/// The four frequencies the weightings are built from, in hertz.
///
/// IEC 61672-1:2013 quotes them rounded, as 20.60, 107.7, 737.9 and 12194,
/// and defines them in its Annex E. With `fr = 1000`, `fL = 10^1.5`,
/// `fH = 10^3.9`, `fA = 10^2.45` and `D = sqrt(1/2)`:
///
/// ```text
/// c  = fL² fH²
/// b  = (fr² + fL² fH² / fr² - D (fL² + fH²)) / (1 - D)
/// f1 = sqrt((-b - sqrt(b² - 4c)) / 2)        f2 = fA (3 - sqrt(5)) / 2
/// f4 = sqrt((-b + sqrt(b² - 4c)) / 2)        f3 = fA (3 + sqrt(5)) / 2
/// ```
///
/// The values here are those formulas worked out once; a test of this
/// module works them out again and compares.
const F1: f64 = 20.598_997_057_568_145;
const F2: f64 = 107.652_648_643_046_27;
const F3: f64 = 737.862_230_736_289_9;
const F4: f64 = 12_194.217_147_998_01;

/// The frequency at which both weightings are 0 dB.
const REFERENCE_FREQUENCY: f64 = 1_000.0;

/// A frequency weighting: how much each frequency counts in a reading.
///
/// The ear is far less sensitive to low and to very high frequencies than
/// to the middle of its range. The A-weighting follows that roughly, for
/// quiet sounds; the C-weighting is nearly flat and keeps the bass. Both
/// are 0 dB at 1 kHz. A level read through one of them is written dB(A) or
/// dB(C).
///
/// The curves are computed from the formulas of IEC 61672-1:2013
/// (equations 6 and 7, with the frequencies of its Annex E), which describe
/// the response at every frequency. Nothing is tabulated or interpolated.
///
/// ```
/// use manguebyte_analysis::Weighting;
///
/// // The values the standard lists, to a tenth of a decibel.
/// assert!((Weighting::A.gain_db(100.0) + 19.1).abs() < 0.05);
/// assert!((Weighting::C.gain_db(100.0) + 0.3).abs() < 0.05);
/// assert_eq!(Weighting::A.gain_db(1_000.0), 0.0);
/// assert_eq!(Weighting::Z.gain_db(31.5), 0.0);
/// ```
///
/// # Limits
///
/// The standard normalises with the rounded constants 2.000 dB and
/// 0.062 dB. Here each curve is divided by its own value at 1 kHz, so that
/// 1 kHz reads exactly 0 dB; the two differ by less than 0.001 dB.
///
/// This is the weighting as a function of frequency, for scaling the bins
/// of a spectrum. It is not a filter for samples: the standard's curve
/// continues above half the sample rate, which no digital filter can
/// follow exactly.
///
/// A later version of this crate may add weightings (B, D, ITU-R 468): a
/// match on this enum needs a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum Weighting {
    /// No weighting: every frequency counts the same. IEC 61672-1 calls
    /// this Z, for zero.
    #[default]
    Z,
    /// The A-weighting: -19.1 dB at 100 Hz, 0 dB at 1 kHz, +1.2 dB at 2 kHz,
    /// -9.3 dB at 20 kHz.
    A,
    /// The C-weighting: -3 dB at 31.5 Hz and at 8 kHz, flat in between.
    C,
}

impl Weighting {
    /// The weighting at `frequency` hertz as a factor on amplitude: 1.0 at
    /// 1 kHz.
    ///
    /// A and C are zero at 0 Hz and fall towards zero as the frequency
    /// grows without limit. A frequency that is not a number gives zero for
    /// them as well. Z is 1.0 for everything.
    pub fn gain(self, frequency: f64) -> f64 {
        match self {
            Self::Z => 1.0,
            Self::A => ratio(a_response, frequency),
            Self::C => ratio(c_response, frequency),
        }
    }

    /// The weighting at `frequency` hertz in decibels: 0.0 at 1 kHz, and
    /// negative infinity where [`gain`](Self::gain) is zero.
    pub fn gain_db(self, frequency: f64) -> f64 {
        20.0 * self.gain(frequency).log10()
    }
}

/// A response curve divided by its value at 1 kHz, and zero where the curve
/// has no value.
fn ratio(response: fn(f64) -> f64, frequency: f64) -> f64 {
    if frequency.is_nan() {
        return 0.0;
    }
    let value = response(frequency) / response(REFERENCE_FREQUENCY);
    // An infinite frequency gives infinity over infinity; the curve's limit
    // there is zero.
    if value.is_finite() { value } else { 0.0 }
}

/// IEC 61672-1:2013 equation 7, without its normalisation constant.
fn a_response(frequency: f64) -> f64 {
    let square = frequency * frequency;
    F4 * F4 * square * square
        / ((square + F1 * F1)
            * ((square + F2 * F2) * (square + F3 * F3)).sqrt()
            * (square + F4 * F4))
}

/// IEC 61672-1:2013 equation 6, without its normalisation constant.
fn c_response(frequency: f64) -> f64 {
    let square = frequency * frequency;
    F4 * F4 * square / ((square + F1 * F1) * (square + F4 * F4))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact frequency of the nominal third-octave band number `n`:
    /// 10 is 10 Hz, 30 is 1 kHz, 43 is 20 kHz.
    fn band(n: i32) -> f64 {
        10f64.powf(f64::from(n) / 10.0)
    }

    #[test]
    fn the_four_frequencies_are_those_of_annex_e() {
        let reference = 1_000.0f64;
        let low_square = 10f64.powf(1.5).powi(2);
        let high_square = 10f64.powf(3.9).powi(2);
        let a = 10f64.powf(2.45);
        let d = 0.5f64.sqrt();
        let c = low_square * high_square;
        let b = (reference * reference + c / (reference * reference)
            - d * (low_square + high_square))
            / (1.0 - d);
        let root = (b * b - 4.0 * c).sqrt();
        // The larger root first. The smaller one is the difference of two
        // nearly equal numbers; the product of the roots is `c`, which gives
        // it without that loss.
        let f4_square = (-b + root) / 2.0;
        let f1_square = c / f4_square;
        let f2 = a * (3.0 - 5f64.sqrt()) / 2.0;
        let f3 = a * (3.0 + 5f64.sqrt()) / 2.0;
        for (name, computed, constant) in [
            ("f1", f1_square.sqrt(), F1),
            ("f2", f2, F2),
            ("f3", f3, F3),
            ("f4", f4_square.sqrt(), F4),
        ] {
            assert!(
                (computed / constant - 1.0).abs() < 1e-12,
                "{name}: {computed} against {constant}"
            );
        }
        // And they round to the figures the standard prints.
        assert_eq!(
            format!("{F1:.2} {F2:.1} {F3:.1} {F4:.0}"),
            "20.60 107.7 737.9 12194"
        );
    }

    #[test]
    fn the_normalisation_is_the_one_the_standard_rounds() {
        // A1000 = -2.000 dB and C1000 = -0.062 dB.
        let a = 20.0 * a_response(1_000.0).log10();
        let c = 20.0 * c_response(1_000.0).log10();
        assert!((a + 2.000).abs() < 0.0005, "{a}");
        assert!((c + 0.062).abs() < 0.0005, "{c}");
        assert_eq!(Weighting::A.gain(1_000.0), 1.0);
        assert_eq!(Weighting::C.gain(1_000.0), 1.0);
    }

    #[test]
    fn the_curves_match_the_table_of_the_standard() {
        // IEC 61672-1:2013, Table 3: the weightings at the nominal
        // third-octave frequencies from 10 Hz to 20 kHz, in decibels.
        const A: [f64; 34] = [
            -70.4, -63.4, -56.7, -50.5, -44.7, -39.4, -34.6, -30.2, -26.2, -22.5, -19.1, -16.1,
            -13.4, -10.9, -8.6, -6.6, -4.8, -3.2, -1.9, -0.8, 0.0, 0.6, 1.0, 1.2, 1.3, 1.2, 1.0,
            0.5, -0.1, -1.1, -2.5, -4.3, -6.6, -9.3,
        ];
        const C: [f64; 34] = [
            -14.3, -11.2, -8.5, -6.2, -4.4, -3.0, -2.0, -1.3, -0.8, -0.5, -0.3, -0.2, -0.1, 0.0,
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -0.1, -0.2, -0.3, -0.5, -0.8, -1.3, -2.0, -3.0,
            -4.4, -6.2, -8.5, -11.2,
        ];
        let mut worst = 0.0f64;
        for (index, (a, c)) in A.iter().zip(C).enumerate() {
            let frequency = band(10 + index as i32);
            for (weighting, expected) in [(Weighting::A, *a), (Weighting::C, c)] {
                let computed = weighting.gain_db(frequency);
                worst = worst.max((computed - expected).abs());
                // The table is rounded to a tenth of a decibel.
                assert!(
                    (computed - expected).abs() < 0.05,
                    "{weighting:?} at {frequency:.1} Hz: {computed:.3} dB, the table says {expected}"
                );
                assert!((weighting.gain(frequency) - 10f64.powf(computed / 20.0)).abs() < 1e-12);
            }
            assert_eq!(Weighting::Z.gain(frequency), 1.0);
        }
        println!("largest distance from the rounded table: {worst:.4} dB");
    }

    #[test]
    fn the_ends_of_the_frequency_axis_have_a_value() {
        for weighting in [Weighting::A, Weighting::C] {
            assert_eq!(weighting.gain(0.0), 0.0);
            assert_eq!(weighting.gain_db(0.0), f64::NEG_INFINITY);
            assert_eq!(weighting.gain(f64::INFINITY), 0.0);
            assert_eq!(weighting.gain(f64::NAN), 0.0);
            assert_eq!(weighting.gain(f64::MAX), 0.0);
            // The curves are even: a negative frequency is its mirror image.
            assert_eq!(weighting.gain(-250.0), weighting.gain(250.0));
            // Far below and far above the audio range they are tiny, finite
            // and not negative.
            for frequency in [1e-300, 1e-3, 1e6, 1e150] {
                let gain = weighting.gain(frequency);
                assert!(
                    (0.0..1e-3).contains(&gain),
                    "{weighting:?} at {frequency}: {gain}"
                );
            }
        }
        assert_eq!(Weighting::Z.gain(f64::NAN), 1.0);
        assert_eq!(Weighting::default(), Weighting::Z);
    }
}
