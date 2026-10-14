//! The K-weighting filter of ITU-R BS.1770, at the sample rate it is printed
//! for and at any other from 1 Hz to 10 MHz.

use core::f64::consts::PI;

use manguebyte_dsp::BiquadCoefficients;

use crate::shared::{MAX_SAMPLE_RATE, MIN_SAMPLE_RATE};

/// The frequency weighting that loudness is measured through.
///
/// Recommendation ITU-R BS.1770-4 defines it as two second-order filters in
/// series: a high shelf of about +4 dB that stands for the acoustic effect
/// of the head, and a high-pass at about 38 Hz (the "RLB" curve). The
/// Recommendation prints their coefficients for 48 kHz only, and asks that
/// other rates "provide the same frequency response".
///
/// [`for_sample_rate`](Self::for_sample_rate) does that without a second
/// table. Each printed filter is read back as the analogue filter it is the
/// bilinear transform of, and that analogue filter is transformed again for
/// the new rate, warped so that the two digital filters agree exactly at
/// the filter's own centre frequency (about 1682 Hz for the shelf and 38 Hz
/// for the high-pass). At 48 kHz the round trip returns the printed
/// coefficients to every digit printed.
///
/// Reading the printed coefficients back into general filter parameters is
/// the approach of B. De Man, "Evaluation of implementations of the EBU
/// R128 loudness measurement", 145th Convention of the Audio Engineering
/// Society, 2018.
///
/// One detail needs care. The printed high-pass has a gain of +0.043 dB in
/// its passband: its numerator is `1, -2, 1`, which is not normalised. That
/// gain is part of the response at 48 kHz, so it is carried to every rate
/// here. Keeping the numerator `1, -2, 1` at every rate instead would let
/// the gain drift with the rate, down to +0.011 dB at 192 kHz.
///
/// # Limits
///
/// The bilinear transform bends the frequency axis more the closer a
/// frequency is to half the sample rate, so the match is exact at 0 Hz and
/// at each centre frequency and approximate between and above. Measured
/// against the 48 kHz response, from 10 Hz up to 20 kHz or 45% of the
/// sample rate: within 0.01 dB at 44.1 kHz and every higher standard rate
/// (0.002 dB at 44.1 kHz, 0.006 dB at 96 kHz, 0.008 dB at 192 kHz), 0.011 dB
/// at 32 kHz, 0.031 dB at 22.05 kHz and 0.29 dB at 8 kHz. The largest
/// difference is always near 2.7 kHz, on the slope of the shelf.
///
/// ```
/// use manguebyte_analysis::KWeighting;
///
/// let at_48k = KWeighting::for_sample_rate(48_000.0);
/// assert!((at_48k.shelf.b0 - 1.535_124_859_586_97).abs() < 5e-15);
///
/// // The constant -0.691 in the loudness formula cancels the gain at 997 Hz.
/// let gain_db = 20.0 * at_48k.magnitude_at(997.0, 48_000.0).log10();
/// assert!((gain_db - 0.691).abs() < 1e-3);
///
/// // The response is the same at another rate.
/// let at_96k = KWeighting::for_sample_rate(96_000.0);
/// let other_db = 20.0 * at_96k.magnitude_at(997.0, 96_000.0).log10();
/// assert!((other_db - gain_db).abs() < 0.01);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KWeighting {
    /// Stage 1: the high shelf that models the head.
    pub shelf: BiquadCoefficients,
    /// Stage 2: the high-pass.
    pub highpass: BiquadCoefficients,
}

impl KWeighting {
    /// The sample rate the Recommendation prints its coefficients for.
    pub const REFERENCE_SAMPLE_RATE: f64 = 48_000.0;

    /// The coefficients exactly as printed in Tables 1 and 2 of
    /// Recommendation ITU-R BS.1770-4, for 48 kHz.
    pub const REFERENCE: Self = Self {
        shelf: BiquadCoefficients {
            b0: 1.535_124_859_586_97,
            b1: -2.691_696_189_406_38,
            b2: 1.198_392_810_852_85,
            a1: -1.690_659_293_182_41,
            a2: 0.732_480_774_215_85,
        },
        highpass: BiquadCoefficients {
            b0: 1.0,
            b1: -2.0,
            b2: 1.0,
            a1: -1.990_047_454_833_98,
            a2: 0.990_072_250_366_21,
        },
    };

    /// The filters for `sample_rate`, in hertz.
    ///
    /// Rates from 1 Hz to 10 MHz are served, and each gives stable filters
    /// with finite coefficients; see the limits above for how far the
    /// response of a low rate is from the reference. A positive rate
    /// outside that range gives the filters of the nearer end of it: far
    /// enough outside, the poles of a filter for the true rate would round
    /// onto the unit circle, or the arithmetic that derives them overflow.
    /// A rate that is not a positive, finite number gives the 48 kHz
    /// filters.
    pub fn for_sample_rate(sample_rate: f64) -> Self {
        let sample_rate = if sample_rate.is_finite() && sample_rate > 0.0 {
            sample_rate.clamp(MIN_SAMPLE_RATE, MAX_SAMPLE_RATE)
        } else {
            Self::REFERENCE_SAMPLE_RATE
        };
        Self {
            shelf: move_to_rate(&Self::REFERENCE.shelf, sample_rate),
            highpass: move_to_rate(&Self::REFERENCE.highpass, sample_rate),
        }
    }

    /// Linear gain of the two filters together at `frequency`.
    pub fn magnitude_at(&self, frequency: f64, sample_rate: f64) -> f64 {
        self.shelf.magnitude_at(frequency, sample_rate)
            * self.highpass.magnitude_at(frequency, sample_rate)
    }
}

/// Redesigns a filter given for 48 kHz so that it has the same response at
/// `sample_rate`, which the caller keeps within the served range. Outside
/// it the stretch below underflows or overflows, and the result is a filter
/// with its poles on the unit circle, or no numbers at all.
fn move_to_rate(reference: &BiquadCoefficients, sample_rate: f64) -> BiquadCoefficients {
    let BiquadCoefficients { b0, b1, b2, a1, a2 } = *reference;

    // Substituting z^-1 = (1 - w) / (1 + w) turns the transfer function into
    // a ratio of two quadratics in w, and on the unit circle
    // w = j tan(pi f / rate). These are their coefficients, highest power
    // first: the analogue filter, on a frequency axis measured in tangents.
    let numerator = [b0 - b1 + b2, 2.0 * (b0 - b2), b0 + b1 + b2];
    let denominator = [1.0 - a1 + a2, 2.0 * (1.0 - a2), 1.0 + a1 + a2];

    // The natural frequency of the poles, where the response is most
    // sensitive, back in hertz.
    let natural =
        KWeighting::REFERENCE_SAMPLE_RATE / PI * (denominator[2] / denominator[0]).sqrt().atan();
    // Below about 3.4 kHz the shelf's centre is past half the sample rate
    // and cannot be matched; the highest frequency that is still safely
    // inside the band takes its place.
    let matched = natural.min(0.45 * sample_rate);

    // One tangent axis is a stretched copy of the other. Choose the stretch
    // that makes the two agree at the matched frequency.
    let stretch = (PI * matched / KWeighting::REFERENCE_SAMPLE_RATE).tan()
        / (PI * matched / sample_rate).tan();
    let stretched = |quadratic: [f64; 3]| {
        [
            quadratic[0] * stretch * stretch,
            quadratic[1] * stretch,
            quadratic[2],
        ]
    };
    let numerator = stretched(numerator);
    let denominator = stretched(denominator);

    // And back to powers of z^-1, with w = (1 - z^-1) / (1 + z^-1).
    let in_z = |quadratic: [f64; 3]| {
        [
            quadratic[0] + quadratic[1] + quadratic[2],
            2.0 * (quadratic[2] - quadratic[0]),
            quadratic[0] - quadratic[1] + quadratic[2],
        ]
    };
    let numerator = in_z(numerator);
    let denominator = in_z(denominator);
    let a0 = denominator[0];
    BiquadCoefficients {
        b0: numerator[0] / a0,
        b1: numerator[1] / a0,
        b2: numerator[2] / a0,
        a1: denominator[1] / a0,
        a2: denominator[2] / a0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn as_array(filter: &BiquadCoefficients) -> [f64; 5] {
        [filter.b0, filter.b1, filter.b2, filter.a1, filter.a2]
    }

    fn db(gain: f64) -> f64 {
        20.0 * gain.log10()
    }

    /// The largest difference in decibels from the 48 kHz response, on a
    /// logarithmic grid from 10 Hz to 20 kHz or 45% of the rate.
    fn worst_mismatch_db(sample_rate: f64) -> f64 {
        let filters = KWeighting::for_sample_rate(sample_rate);
        let top = 20_000.0f64.min(0.45 * sample_rate);
        let mut worst = 0.0f64;
        for step in 0..=2_000 {
            let frequency = 10.0 * (top / 10.0).powf(f64::from(step) / 2_000.0);
            let here = filters.magnitude_at(frequency, sample_rate);
            let reference =
                KWeighting::REFERENCE.magnitude_at(frequency, KWeighting::REFERENCE_SAMPLE_RATE);
            worst = worst.max((db(here) - db(reference)).abs());
        }
        worst
    }

    #[test]
    fn at_48_khz_the_coefficients_are_the_ones_the_recommendation_prints() {
        let derived = KWeighting::for_sample_rate(48_000.0);
        // The tables give 14 decimal places, so half a unit of the last one.
        for (derived, printed) in [
            (derived.shelf, KWeighting::REFERENCE.shelf),
            (derived.highpass, KWeighting::REFERENCE.highpass),
        ] {
            for (derived, printed) in as_array(&derived).iter().zip(as_array(&printed)) {
                assert!(
                    (derived - printed).abs() < 5e-15,
                    "{derived:.17} is not {printed:.14}"
                );
            }
        }
        // Spelled out once more against the digits themselves.
        assert!((derived.shelf.b0 - 1.53512485958697).abs() < 5e-15);
        assert!((derived.shelf.b1 + 2.69169618940638).abs() < 5e-15);
        assert!((derived.shelf.b2 - 1.19839281085285).abs() < 5e-15);
        assert!((derived.shelf.a1 + 1.69065929318241).abs() < 5e-15);
        assert!((derived.shelf.a2 - 0.73248077421585).abs() < 5e-15);
        assert!((derived.highpass.b0 - 1.0).abs() < 5e-15);
        assert!((derived.highpass.b1 + 2.0).abs() < 5e-15);
        assert!((derived.highpass.b2 - 1.0).abs() < 5e-15);
        assert!((derived.highpass.a1 + 1.99004745483398).abs() < 5e-15);
        assert!((derived.highpass.a2 - 0.99007225036621).abs() < 5e-15);
    }

    #[test]
    fn the_gain_at_997_hz_is_what_the_loudness_constant_cancels() {
        // BS.1770-4, Annex 1, note 1.
        let gain = KWeighting::REFERENCE.magnitude_at(997.0, 48_000.0);
        assert!((db(gain) - 0.691).abs() < 1e-3, "{}", db(gain));
        // The shape of the whole curve: 4 dB up at the top, 6 dB down at
        // the high-pass's own frequency of 38 Hz.
        assert!((db(KWeighting::REFERENCE.magnitude_at(10_000.0, 48_000.0)) - 4.04).abs() < 0.01);
        assert!((db(KWeighting::REFERENCE.magnitude_at(38.0, 48_000.0)) + 6.0).abs() < 0.01);
    }

    #[test]
    fn every_standard_rate_has_the_response_of_48_khz() {
        // The largest difference in decibels anywhere from 10 Hz to 20 kHz.
        for (rate, limit) in [
            (44_100.0, 0.002),
            (48_000.0, 1e-12),
            (88_200.0, 0.0065),
            (96_000.0, 0.0065),
            (176_400.0, 0.008),
            (192_000.0, 0.008),
            (384_000.0, 0.0085),
        ] {
            let worst = worst_mismatch_db(rate);
            assert!(worst < limit, "{rate} Hz is {worst:.4} dB away");
        }
    }

    #[test]
    fn low_rates_are_close_and_say_how_close() {
        for (rate, limit) in [(32_000.0, 0.011), (22_050.0, 0.031), (8_000.0, 0.29)] {
            let worst = worst_mismatch_db(rate);
            assert!(worst < limit, "{rate} Hz is {worst:.4} dB away");
        }
    }

    #[test]
    fn the_passband_gain_of_the_high_pass_does_not_drift_with_the_rate() {
        let reference = db(KWeighting::REFERENCE
            .highpass
            .magnitude_at(5_000.0, 48_000.0));
        // The printed filter is not normalised: its passband sits a little
        // above 0 dB.
        assert!((reference - 0.043).abs() < 0.001, "{reference}");
        for rate in [44_100.0, 96_000.0, 192_000.0] {
            let here = db(KWeighting::for_sample_rate(rate)
                .highpass
                .magnitude_at(5_000.0, rate));
            assert!((here - reference).abs() < 1e-4, "{rate}: {here}");
        }
    }

    #[test]
    fn every_rate_gives_a_stable_filter() {
        for rate in [
            1.0,
            100.0,
            1_000.0,
            3_000.0,
            8_000.0,
            11_025.0,
            47_952.047_952,
            768_000.0,
            1e7,
        ] {
            let filters = KWeighting::for_sample_rate(rate);
            for filter in [filters.shelf, filters.highpass] {
                assert!(as_array(&filter).iter().all(|value| value.is_finite()));
                // Both poles inside the unit circle.
                assert!(filter.a2.abs() < 1.0, "{rate}: {filter:?}");
                assert!(filter.a1.abs() < 1.0 + filter.a2, "{rate}: {filter:?}");
            }
        }
    }

    /// How far inside the unit circle the poles of `filter` are, as the
    /// smallest of the three margins of the stability triangle: `1 - a2`,
    /// `1 + a1 + a2` and `1 - a1 + a2`. All three are positive exactly
    /// when both poles are inside.
    fn stability_margin(filter: &BiquadCoefficients) -> f64 {
        (1.0 - filter.a2)
            .min(1.0 + filter.a1 + filter.a2)
            .min(1.0 - filter.a1 + filter.a2)
    }

    #[test]
    fn every_rate_in_the_served_range_gives_a_stable_filter() {
        // Six thousand rates from 1 Hz to 10 MHz, each 0.27% above the one
        // before, and the two ends themselves.
        let steps = 6_000;
        let mut smallest = f64::INFINITY;
        for step in 0..=steps {
            let rate = if step == steps {
                MAX_SAMPLE_RATE
            } else {
                MIN_SAMPLE_RATE
                    * (MAX_SAMPLE_RATE / MIN_SAMPLE_RATE).powf(f64::from(step) / f64::from(steps))
            };
            let filters = KWeighting::for_sample_rate(rate);
            for filter in [filters.shelf, filters.highpass] {
                assert!(
                    as_array(&filter).iter().all(|value| value.is_finite()),
                    "{rate}: {filter:?}"
                );
                let margin = stability_margin(&filter);
                assert!(margin > 0.0, "{rate}: {filter:?}");
                smallest = smallest.min(margin);
            }
        }
        // The closest any pole comes to the circle is at 10 MHz, where the
        // high-pass at 38 Hz has `1 + a1 + a2` near 6e-10: more than two
        // million times the spacing of the numbers it is computed from.
        println!("smallest stability margin: {smallest:e}");
        assert!(smallest > 2e-10, "{smallest:e}");
    }

    #[test]
    fn a_rate_outside_the_served_range_gives_the_filters_of_its_nearer_end() {
        let lowest = KWeighting::for_sample_rate(MIN_SAMPLE_RATE);
        let highest = KWeighting::for_sample_rate(MAX_SAMPLE_RATE);
        assert_eq!((MIN_SAMPLE_RATE, MAX_SAMPLE_RATE), (1.0, 1e7));
        // The smallest and the largest positive numbers there are, and
        // some in between. Without the limit, 1e-100 Hz and 1e100 Hz put
        // both poles on the unit circle and 1e160 Hz gives no numbers at
        // all.
        for rate in [
            5e-324,
            f64::MIN_POSITIVE,
            1e-300,
            1e-100,
            1e-9,
            0.5,
            0.999_999,
        ] {
            assert_eq!(KWeighting::for_sample_rate(rate), lowest, "{rate:e}");
        }
        for rate in [1.000_001e7, 1e9, 1e100, 1e160, 1e300, f64::MAX] {
            assert_eq!(KWeighting::for_sample_rate(rate), highest, "{rate:e}");
        }
        for filter in [
            lowest.shelf,
            lowest.highpass,
            highest.shelf,
            highest.highpass,
        ] {
            assert!(as_array(&filter).iter().all(|value| value.is_finite()));
            assert!(stability_margin(&filter) > 0.0, "{filter:?}");
        }
    }

    #[test]
    fn a_rate_that_is_not_a_rate_gives_the_reference() {
        let reference = KWeighting::for_sample_rate(48_000.0);
        for rate in [0.0, -44_100.0, f64::NAN, f64::INFINITY] {
            assert_eq!(KWeighting::for_sample_rate(rate), reference);
        }
    }
}
