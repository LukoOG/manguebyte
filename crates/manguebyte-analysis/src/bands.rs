//! Fractional-octave bands with the mid-band frequencies of IEC 61260-1.

/// The reference frequency of the band system, in hertz.
const REFERENCE_HZ: f64 = 1_000.0;

/// The base-ten octave ratio of IEC 61260-1 is `G = 10^(3/10)`; this is
/// `log10(G)`.
const LOG10_OCTAVE: f64 = 0.3;

/// The ten preferred numbers of the R10 series (ISO 3), from which the
/// nominal mid-band frequencies of octave and one-third-octave bands are
/// taken: 1.00, 1.25, 1.60 and so on, one per third of an octave.
const R10: [f64; 10] = [1.0, 1.25, 1.6, 2.0, 2.5, 3.15, 4.0, 5.0, 6.3, 8.0];

/// The widest range of frequencies bands are made for, in hertz. Asking
/// for more is taken as asking for this.
const LOWEST_HZ: f64 = 0.1;
const HIGHEST_HZ: f64 = 1e7;

/// How wide a fractional-octave band is: one octave, or a third, sixth,
/// twelfth or twenty-fourth of one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OctaveFraction {
    /// Octave bands.
    Whole,
    /// One-third-octave bands, the usual choice for acoustics.
    Third,
    /// One-sixth-octave bands.
    Sixth,
    /// One-twelfth-octave bands, about a semitone wide.
    Twelfth,
    /// One-twenty-fourth-octave bands, about a quarter tone wide.
    TwentyFourth,
}

impl OctaveFraction {
    /// The `b` of IEC 61260-1: 1, 3, 6, 12 or 24 bands per octave.
    pub fn bands_per_octave(self) -> u32 {
        match self {
            Self::Whole => 1,
            Self::Third => 3,
            Self::Sixth => 6,
            Self::Twelfth => 12,
            Self::TwentyFourth => 24,
        }
    }

    /// Every band from the one that contains `low_hz` to the one that
    /// contains `high_hz`. Allocates.
    ///
    /// The two limits are swapped if they come in the wrong order, and
    /// kept within 0.1 Hz to 10 MHz; one that is not a positive number is
    /// taken as the nearer of 20 Hz and 20 kHz.
    ///
    /// ```
    /// use manguebyte_analysis::OctaveFraction;
    ///
    /// let thirds = OctaveFraction::Third.bands(20.0, 20_000.0);
    /// assert_eq!(thirds.len(), 31);
    /// assert_eq!(thirds[0].nominal(), 20.0);
    /// assert_eq!(thirds[17].nominal(), 1_000.0);
    /// assert_eq!(thirds[30].nominal(), 20_000.0);
    /// ```
    pub fn bands(self, low_hz: f64, high_hz: f64) -> Vec<Band> {
        let usable = |frequency: f64, fallback: f64| {
            if frequency.is_finite() && frequency > 0.0 {
                frequency.clamp(LOWEST_HZ, HIGHEST_HZ)
            } else {
                fallback
            }
        };
        let low = usable(low_hz, 20.0);
        let high = usable(high_hz, 20_000.0);
        let (low, high) = if low <= high {
            (low, high)
        } else {
            (high, low)
        };
        let first = Band::containing(self, low).index();
        let last = Band::containing(self, high).index();
        (first..=last).map(|index| Band::new(self, index)).collect()
    }
}

/// One fractional-octave band, with the exact frequencies IEC 61260-1:2014
/// defines and a nominal frequency to label it with.
///
/// The standard uses the base-ten octave ratio `G = 10^(3/10)`, about
/// 1.9953, and the reference frequency 1 kHz. Band number `x` of a system
/// with `b` bands per octave has its exact mid-band frequency at
///
/// ```text
/// fm = 1000 G^(x / b)            when b is odd (octaves, thirds)
/// fm = 1000 G^((2x + 1) / (2b))  when b is even (sixths and narrower)
/// ```
///
/// and its edges at `fm G^(-1/(2b))` and `fm G^(1/(2b))`. With an odd `b`
/// 1 kHz is the middle of band 0; with an even `b` it is the edge between
/// bands -1 and 0. Consecutive bands share their edges exactly, so the
/// bands of a range cover it without gap or overlap, and an octave band
/// covers exactly the three third-octave bands around it.
///
/// ```
/// use manguebyte_analysis::{Band, OctaveFraction};
///
/// let band = Band::new(OctaveFraction::Third, 1);
/// assert_eq!(band.nominal(), 1_250.0);
/// assert!((band.centre() - 1_258.925).abs() < 0.001);
/// assert!((band.upper() / band.lower() - 10f64.powf(0.1)).abs() < 1e-12);
/// ```
///
/// # Nominal frequencies
///
/// IEC 61260-1 tabulates nominal mid-band frequencies (31.5 Hz, 63 Hz, and
/// so on) for octave and one-third-octave bands only. Those are the
/// preferred numbers of the R10 series, which is how they are computed
/// here, for any band number. For the narrower bands the standard gives
/// none; [`nominal`](Self::nominal) is then the exact mid-band frequency
/// rounded to three significant figures, a convention of this crate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Band {
    fraction: OctaveFraction,
    index: i32,
    lower: f64,
    centre: f64,
    upper: f64,
}

impl Band {
    /// Band number `index` of the system `fraction`: 0 is the band at
    /// 1 kHz (just above it for an even number of bands per octave), and
    /// each step is one band.
    pub fn new(fraction: OctaveFraction, index: i32) -> Self {
        let b = f64::from(fraction.bands_per_octave());
        let x = f64::from(index);
        let at = |exponent: f64| REFERENCE_HZ * 10f64.powf(LOG10_OCTAVE * exponent);
        // The edges are written so that the upper edge of one band and the
        // lower edge of the next are the same expression, to the bit.
        let (lower, centre, upper) = if fraction.bands_per_octave() % 2 == 1 {
            (at((x - 0.5) / b), at(x / b), at(((x + 1.0) - 0.5) / b))
        } else {
            (
                at(x / b),
                at((2.0 * x + 1.0) / (2.0 * b)),
                at((x + 1.0) / b),
            )
        };
        Self {
            fraction,
            index,
            lower,
            centre,
            upper,
        }
    }

    /// The band of the system `fraction` that contains `frequency`, which
    /// belongs to the band whose lower edge it is. A frequency that is not
    /// a positive number is taken as 1 kHz, and one outside 0.1 Hz to
    /// 10 MHz as the nearer end of that range.
    pub fn containing(fraction: OctaveFraction, frequency: f64) -> Self {
        let frequency = if frequency.is_finite() && frequency > 0.0 {
            frequency.clamp(LOWEST_HZ, HIGHEST_HZ)
        } else {
            REFERENCE_HZ
        };
        let b = f64::from(fraction.bands_per_octave());
        let position = b * (frequency / REFERENCE_HZ).log10() / LOG10_OCTAVE;
        let offset = if fraction.bands_per_octave() % 2 == 1 {
            0.5
        } else {
            0.0
        };
        // The clamp above keeps this within a few hundred bands of zero.
        let mut band = Self::new(fraction, (position + offset).floor() as i32);
        // The logarithm can land on the wrong side of an edge by rounding.
        if frequency < band.lower {
            band = Self::new(fraction, band.index - 1);
        } else if frequency >= band.upper {
            band = Self::new(fraction, band.index + 1);
        }
        band
    }

    /// The band system this band belongs to.
    pub fn fraction(&self) -> OctaveFraction {
        self.fraction
    }

    /// The band number, as in [`new`](Self::new).
    pub fn index(&self) -> i32 {
        self.index
    }

    /// The exact lower edge, in hertz.
    pub fn lower(&self) -> f64 {
        self.lower
    }

    /// The exact mid-band frequency, in hertz: the geometric mean of the
    /// two edges.
    pub fn centre(&self) -> f64 {
        self.centre
    }

    /// The exact upper edge, in hertz.
    pub fn upper(&self) -> f64 {
        self.upper
    }

    /// The width of the band, in hertz.
    pub fn width(&self) -> f64 {
        self.upper - self.lower
    }

    /// The nominal mid-band frequency to label the band with, in hertz:
    /// 31.5, 1000, 1250 and so on. See [Nominal frequencies](Self#nominal-frequencies).
    pub fn nominal(&self) -> f64 {
        match self.fraction {
            OctaveFraction::Whole => preferred(3 * i64::from(self.index)),
            OctaveFraction::Third => preferred(i64::from(self.index)),
            _ => three_figures(self.centre),
        }
    }
}

/// The R10 preferred number `steps` thirds of an octave from 1 kHz.
fn preferred(steps: i64) -> f64 {
    let decade = steps.div_euclid(10);
    let step = steps.rem_euclid(10) as usize;
    // Multiplying by a power of ten above one, or dividing by one, keeps
    // values such as 31.5 and 0.125 as close as an f64 can hold them.
    let power = 3 + decade;
    let scale = 10f64.powi(power.unsigned_abs().min(400) as i32);
    if power >= 0 {
        R10[step] * scale
    } else {
        R10[step] / scale
    }
}

/// `value` rounded to three significant figures.
fn three_figures(value: f64) -> f64 {
    if !(value.is_finite() && value > 0.0) {
        return value;
    }
    let digits = value.log10().floor() as i32 - 2;
    let scale = 10f64.powi(digits.abs());
    if digits >= 0 {
        (value / scale).round() * scale
    } else {
        (value * scale).round() / scale
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [OctaveFraction; 5] = [
        OctaveFraction::Whole,
        OctaveFraction::Third,
        OctaveFraction::Sixth,
        OctaveFraction::Twelfth,
        OctaveFraction::TwentyFourth,
    ];

    #[test]
    fn nominal_frequencies_are_those_the_standard_lists() {
        // IEC 61260-1:2014, Annex E: the nominal one-third-octave
        // mid-band frequencies of the audio range, and the octave ones.
        let thirds = [
            20.0, 25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0, 315.0,
            400.0, 500.0, 630.0, 800.0, 1_000.0, 1_250.0, 1_600.0, 2_000.0, 2_500.0, 3_150.0,
            4_000.0, 5_000.0, 6_300.0, 8_000.0, 10_000.0, 12_500.0, 16_000.0, 20_000.0,
        ];
        let measured: Vec<f64> = OctaveFraction::Third
            .bands(20.0, 20_000.0)
            .iter()
            .map(Band::nominal)
            .collect();
        assert_eq!(measured, thirds);

        let octaves = [
            16.0, 31.5, 63.0, 125.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0,
        ];
        let measured: Vec<f64> = OctaveFraction::Whole
            .bands(20.0, 20_000.0)
            .iter()
            .map(Band::nominal)
            .collect();
        assert_eq!(measured, octaves);

        // Far from the audio range the series carries on.
        assert_eq!(Band::new(OctaveFraction::Third, -30).nominal(), 1.0);
        assert_eq!(Band::new(OctaveFraction::Third, -25).nominal(), 3.15);
        assert_eq!(Band::new(OctaveFraction::Third, 21).nominal(), 125_000.0);
        // Every nominal value is within 1.5% of the exact one, as
        // preferred numbers are.
        for band in OctaveFraction::Third.bands(1.0, 1e6) {
            assert!(
                (band.nominal() / band.centre() - 1.0).abs() < 0.015,
                "{band:?}"
            );
        }
    }

    #[test]
    fn exact_frequencies_follow_the_defining_formulas() {
        let g = 10f64.powf(0.3);
        for fraction in ALL {
            let b = f64::from(fraction.bands_per_octave());
            for index in -40..=40 {
                let band = Band::new(fraction, index);
                let x = f64::from(index);
                let expected = if fraction.bands_per_octave() % 2 == 1 {
                    1_000.0 * g.powf(x / b)
                } else {
                    1_000.0 * g.powf((2.0 * x + 1.0) / (2.0 * b))
                };
                assert!((band.centre() / expected - 1.0).abs() < 1e-13, "{band:?}");
                // The mid-band frequency is the geometric mean of the edges,
                // and the edges are G^(1/b) apart.
                assert!((band.centre() / (band.lower() * band.upper()).sqrt() - 1.0).abs() < 1e-13);
                assert!((band.upper() / band.lower() / g.powf(1.0 / b) - 1.0).abs() < 1e-13);
                // Neighbours share their edge to the bit.
                assert_eq!(
                    band.upper().to_bits(),
                    Band::new(fraction, index + 1).lower().to_bits()
                );
                assert_eq!(band.index(), index);
                assert_eq!(band.fraction(), fraction);
                assert!(band.width() > 0.0);
            }
        }
        // 1 kHz is the middle of a band for odd b and an edge for even b.
        assert_eq!(Band::new(OctaveFraction::Third, 0).centre(), 1_000.0);
        assert_eq!(Band::new(OctaveFraction::Whole, 0).centre(), 1_000.0);
        for fraction in [
            OctaveFraction::Sixth,
            OctaveFraction::Twelfth,
            OctaveFraction::TwentyFourth,
        ] {
            assert_eq!(Band::new(fraction, 0).lower(), 1_000.0);
        }
        // An octave band is exactly three third-octave bands.
        for index in -5..=5 {
            let octave = Band::new(OctaveFraction::Whole, index);
            assert_eq!(
                octave.lower(),
                Band::new(OctaveFraction::Third, 3 * index - 1).lower()
            );
            assert_eq!(
                octave.upper(),
                Band::new(OctaveFraction::Third, 3 * index + 1).upper()
            );
        }
    }

    #[test]
    fn every_frequency_belongs_to_exactly_one_band() {
        for fraction in ALL {
            let bands = fraction.bands(20.0, 20_000.0);
            assert!(bands[0].lower() <= 20.0 && 20.0 < bands[0].upper());
            let last = bands[bands.len() - 1];
            assert!(last.lower() <= 20_000.0 && 20_000.0 < last.upper());
            for band in &bands {
                for frequency in [band.lower(), band.centre(), band.upper() * (1.0 - 1e-15)] {
                    assert_eq!(Band::containing(fraction, frequency), *band);
                }
            }
            println!("{fraction:?}: {} bands from 20 Hz to 20 kHz", bands.len());
        }
        assert_eq!(OctaveFraction::Sixth.bands(20.0, 20_000.0).len(), 61);
        // Bands -68 to 52 and -136 to 104.
        assert_eq!(OctaveFraction::Twelfth.bands(20.0, 20_000.0).len(), 121);
        assert_eq!(
            OctaveFraction::TwentyFourth.bands(20.0, 20_000.0).len(),
            241
        );
    }

    #[test]
    fn odd_limits_give_a_sensible_range() {
        let normal = OctaveFraction::Third.bands(20.0, 20_000.0);
        assert_eq!(OctaveFraction::Third.bands(20_000.0, 20.0), normal);
        assert_eq!(OctaveFraction::Third.bands(f64::NAN, -1.0), normal);
        let widest = OctaveFraction::TwentyFourth.bands(0.0, f64::INFINITY);
        assert_eq!(widest, OctaveFraction::TwentyFourth.bands(20.0, 20_000.0));
        let huge = OctaveFraction::TwentyFourth.bands(1e-300, 1e300);
        assert!(huge.len() < 700, "{}", huge.len());
        assert_eq!(OctaveFraction::Third.bands(1_000.0, 1_000.0).len(), 1);
        assert_eq!(
            Band::containing(OctaveFraction::Third, f64::NAN).nominal(),
            1_000.0
        );
    }

    #[test]
    fn narrow_bands_are_labelled_with_three_figures() {
        assert_eq!(three_figures(1_059.463_094), 1_060.0);
        assert_eq!(three_figures(19.952_623), 20.0);
        assert_eq!(three_figures(0.012_345), 0.0123);
        assert_eq!(Band::new(OctaveFraction::Sixth, 0).nominal(), 1_060.0);
        assert_eq!(Band::new(OctaveFraction::Sixth, -1).nominal(), 944.0);
    }
}
