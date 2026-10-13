/// How a continuous parameter maps between its real value and the `0..=1`
/// position that hosts, automation lanes and controls work with.
///
/// The mapping decides how a knob feels. A frequency control needs most of
/// its travel in the low range, so it uses [`logarithmic`](Self::logarithmic);
/// a mix control is fine with [`linear`](Self::linear).
///
/// ```
/// use manguebyte_params::FloatRange;
///
/// let cutoff = FloatRange::logarithmic(20.0, 20_000.0);
/// // Halfway round the knob is the geometric middle, not 10 kHz.
/// assert!((cutoff.denormalize(0.5) - 632.46).abs() < 0.01);
/// assert!((cutoff.normalize(632.46) - 0.5).abs() < 1e-5);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FloatRange {
    /// Equal steps of position are equal steps of value.
    Linear {
        /// Value at position 0.
        min: f32,
        /// Value at position 1.
        max: f32,
    },
    /// A power curve. A `factor` below 1 gives the low end of the range more
    /// of the travel, above 1 gives it less.
    Skewed {
        /// Value at position 0.
        min: f32,
        /// Value at position 1.
        max: f32,
        /// Exponent applied to the linear position.
        factor: f32,
    },
    /// A power curve mirrored around the middle of the range, for two-sided
    /// parameters such as pan or detune. A `factor` below 1 gives the centre
    /// more of the travel.
    SymmetricSkewed {
        /// Value at position 0.
        min: f32,
        /// Value at position 1.
        max: f32,
        /// Exponent applied to the distance from the centre.
        factor: f32,
    },
    /// Equal steps of position are equal ratios of value. Both ends must be
    /// positive.
    Logarithmic {
        /// Value at position 0.
        min: f32,
        /// Value at position 1.
        max: f32,
    },
}

impl FloatRange {
    /// A linear range.
    ///
    /// # Panics
    ///
    /// Panics unless `min < max` and both are finite.
    pub fn linear(min: f32, max: f32) -> Self {
        check_bounds(min, max);
        Self::Linear { min, max }
    }

    /// A power-curve range. See [`FloatRange::Skewed`].
    ///
    /// # Panics
    ///
    /// Panics unless `min < max`, both are finite and `factor` is positive.
    pub fn skewed(min: f32, max: f32, factor: f32) -> Self {
        check_bounds(min, max);
        check_factor(factor);
        Self::Skewed { min, max, factor }
    }

    /// A power-curve range shaped so that `centre` sits at position 0.5.
    ///
    /// # Panics
    ///
    /// Panics unless `min < centre < max` and all are finite.
    pub fn skewed_around(min: f32, max: f32, centre: f32) -> Self {
        check_bounds(min, max);
        assert!(
            min < centre && centre < max,
            "centre must lie strictly inside the range"
        );
        let proportion = f64::from(centre - min) / f64::from(max - min);
        let factor = (0.5f64.ln() / proportion.ln()) as f32;
        Self::Skewed { min, max, factor }
    }

    /// A mirrored power-curve range. See [`FloatRange::SymmetricSkewed`].
    ///
    /// # Panics
    ///
    /// Panics unless `min < max`, both are finite and `factor` is positive.
    pub fn symmetric_skewed(min: f32, max: f32, factor: f32) -> Self {
        check_bounds(min, max);
        check_factor(factor);
        Self::SymmetricSkewed { min, max, factor }
    }

    /// A logarithmic range.
    ///
    /// # Panics
    ///
    /// Panics unless `0 < min < max` and both are finite.
    pub fn logarithmic(min: f32, max: f32) -> Self {
        check_bounds(min, max);
        assert!(min > 0.0, "a logarithmic range must start above zero");
        Self::Logarithmic { min, max }
    }

    /// The value at position 0.
    pub fn min(&self) -> f32 {
        match *self {
            Self::Linear { min, .. }
            | Self::Skewed { min, .. }
            | Self::SymmetricSkewed { min, .. }
            | Self::Logarithmic { min, .. } => min,
        }
    }

    /// The value at position 1.
    pub fn max(&self) -> f32 {
        match *self {
            Self::Linear { max, .. }
            | Self::Skewed { max, .. }
            | Self::SymmetricSkewed { max, .. }
            | Self::Logarithmic { max, .. } => max,
        }
    }

    /// Limits `value` to the range.
    pub fn clamp(&self, value: f32) -> f32 {
        value.clamp(self.min(), self.max())
    }

    /// Maps a value to its position in `0..=1`. Values outside the range are
    /// clamped first.
    pub fn normalize(&self, value: f32) -> f64 {
        let (min, max) = (f64::from(self.min()), f64::from(self.max()));
        let value = f64::from(self.clamp(value));
        let linear = (value - min) / (max - min);
        let position = match *self {
            Self::Linear { .. } => linear,
            Self::Skewed { factor, .. } => linear.powf(f64::from(factor)),
            Self::SymmetricSkewed { factor, .. } => {
                let distance = 2.0 * linear - 1.0;
                0.5 * (1.0 + distance.abs().powf(f64::from(factor)).copysign(distance))
            }
            Self::Logarithmic { .. } => (value / min).ln() / (max / min).ln(),
        };
        position.clamp(0.0, 1.0)
    }

    /// Maps a position in `0..=1` to a value. Positions outside `0..=1` are
    /// clamped first.
    pub fn denormalize(&self, position: f64) -> f32 {
        let (min, max) = (f64::from(self.min()), f64::from(self.max()));
        let position = position.clamp(0.0, 1.0);
        let value = match *self {
            Self::Linear { .. } => min + (max - min) * position,
            Self::Skewed { factor, .. } => {
                min + (max - min) * position.powf(1.0 / f64::from(factor))
            }
            Self::SymmetricSkewed { factor, .. } => {
                let distance = 2.0 * position - 1.0;
                let linear = 0.5
                    * (1.0
                        + distance
                            .abs()
                            .powf(1.0 / f64::from(factor))
                            .copysign(distance));
                min + (max - min) * linear
            }
            Self::Logarithmic { .. } => min * (max / min).powf(position),
        };
        self.clamp(value as f32)
    }
}

fn check_bounds(min: f32, max: f32) {
    assert!(
        min.is_finite() && max.is_finite() && min < max,
        "a range needs finite bounds with min < max, got {min}..{max}"
    );
}

fn check_factor(factor: f32) {
    assert!(
        factor.is_finite() && factor > 0.0,
        "a skew factor must be positive, got {factor}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all() -> [FloatRange; 5] {
        [
            FloatRange::linear(-24.0, 24.0),
            FloatRange::skewed(0.0, 10.0, 0.3),
            FloatRange::skewed_around(20.0, 20_000.0, 1_000.0),
            FloatRange::symmetric_skewed(-100.0, 100.0, 0.5),
            FloatRange::logarithmic(20.0, 20_000.0),
        ]
    }

    #[test]
    fn ends_map_to_ends() {
        for range in all() {
            assert_eq!(range.normalize(range.min()), 0.0, "{range:?}");
            assert_eq!(range.normalize(range.max()), 1.0, "{range:?}");
            assert_eq!(range.denormalize(0.0), range.min(), "{range:?}");
            assert_eq!(range.denormalize(1.0), range.max(), "{range:?}");
        }
    }

    #[test]
    fn round_trips_and_is_monotonic() {
        for range in all() {
            let mut previous = range.min();
            for step in 0..=100 {
                let position = f64::from(step) / 100.0;
                let value = range.denormalize(position);
                assert!(value >= previous, "{range:?} went down at {position}");
                assert!(
                    (range.normalize(value) - position).abs() < 1e-4,
                    "{range:?} at {position}"
                );
                previous = value;
            }
        }
    }

    #[test]
    fn out_of_range_input_is_clamped() {
        for range in all() {
            assert_eq!(range.normalize(f32::MAX), 1.0);
            assert_eq!(range.normalize(f32::MIN), 0.0);
            assert_eq!(range.denormalize(7.0), range.max());
            assert_eq!(range.denormalize(-7.0), range.min());
        }
    }

    #[test]
    fn skewed_around_puts_the_centre_in_the_middle() {
        let range = FloatRange::skewed_around(20.0, 20_000.0, 1_000.0);
        assert!((range.denormalize(0.5) - 1_000.0).abs() < 0.5);
    }

    #[test]
    fn symmetric_skew_keeps_the_centre_and_mirrors() {
        let range = FloatRange::symmetric_skewed(-100.0, 100.0, 0.5);
        assert!(range.denormalize(0.5).abs() < 1e-4);
        let low = range.denormalize(0.25);
        let high = range.denormalize(0.75);
        assert!((low + high).abs() < 1e-3);
        // A factor below one keeps values near the centre for longer.
        assert!(high < 50.0);
    }

    #[test]
    fn logarithmic_gives_each_octave_the_same_travel() {
        let range = FloatRange::logarithmic(20.0, 20_000.0);
        let octave = range.normalize(200.0) - range.normalize(100.0);
        let higher_octave = range.normalize(8_000.0) - range.normalize(4_000.0);
        assert!((octave - higher_octave).abs() < 1e-6);
    }

    #[test]
    #[should_panic(expected = "min < max")]
    fn refuses_an_empty_range() {
        let _ = FloatRange::linear(1.0, 1.0);
    }
}
