//! What a meter shares with the threads that read it.
//!
//! Every meter keeps its readings in atomics behind an `Arc`. The audio
//! thread stores, any other thread loads, and neither ever waits for the
//! other. A reader may see values from two consecutive blocks side by side;
//! for a display that is harmless.

use core::sync::atomic::{AtomicBool, Ordering};

use manguebyte_core::AtomicF64;
use manguebyte_core::decibels::gain_to_db;

/// One number per channel.
#[derive(Debug)]
pub(crate) struct Row(Box<[AtomicF64]>);

impl Row {
    /// A row of `len` zeros.
    pub(crate) fn new(len: usize) -> Self {
        Self((0..len).map(|_| AtomicF64::new(0.0)).collect())
    }

    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }

    /// The value for `index`, or zero for a channel the meter does not have.
    pub(crate) fn get(&self, index: usize) -> f64 {
        self.0
            .get(index)
            .map_or(0.0, |value| value.load(Ordering::Relaxed))
    }

    pub(crate) fn set(&self, index: usize, value: f64) {
        if let Some(slot) = self.0.get(index) {
            slot.store(value, Ordering::Relaxed);
        }
    }

    /// The largest value in the row, or zero for an empty row.
    pub(crate) fn max(&self) -> f64 {
        self.0
            .iter()
            .map(|value| value.load(Ordering::Relaxed))
            .fold(0.0, f64::max)
    }
}

/// A request from a reader that the audio thread picks up at its next block.
#[derive(Debug, Default)]
pub(crate) struct Flag(AtomicBool);

impl Flag {
    pub(crate) fn raise(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Whether the flag was raised, lowering it in the same step.
    pub(crate) fn take(&self) -> bool {
        self.0.swap(false, Ordering::AcqRel)
    }
}

/// Linear amplitude to decibels, with the framework's floor of -100 dB.
pub(crate) fn db(amplitude: f64) -> f64 {
    gain_to_db(amplitude)
}

/// The largest magnitude a meter measures: 180 dB above full scale.
///
/// Nothing real is that loud. With the limit, the squares, sums and
/// products the meters form stay far inside the range of an `f64`, so no
/// input, however broken, can leave an infinity or a NaN behind.
pub(crate) const INPUT_LIMIT: f64 = 1e9;

/// A sample as every meter takes it in: one that is not finite (a NaN or
/// an infinity) is read as silence, and a finite one beyond
/// [`INPUT_LIMIT`] as the limit.
///
/// One NaN in a recursive filter stays there for ever, and so does the
/// infinity that the square of a large enough finite sample turns into. A
/// meter that has to be reset after a glitch upstream is of little use, so
/// the bad sample is stopped at the door. Every meter passes each sample
/// through here before anything else is done with it.
#[inline]
pub(crate) fn screened(sample: f64) -> f64 {
    if sample.is_finite() {
        sample.clamp(-INPUT_LIMIT, INPUT_LIMIT)
    } else {
        0.0
    }
}

/// The per-sample factor of a one-pole smoother with the given time
/// constant: after `seconds` a step has covered `1 - 1/e` of the distance.
pub(crate) fn smoothing_factor(seconds: f64, sample_rate: f64) -> f64 {
    let samples = seconds * sample_rate;
    if samples > 0.0 {
        1.0 - (-1.0 / samples).exp()
    } else {
        1.0
    }
}

/// A sample rate the meters can work at: the given one if it is a positive
/// number, 44.1 kHz otherwise, and never outside
/// [`MIN_SAMPLE_RATE`]`..=`[`MAX_SAMPLE_RATE`].
pub(crate) fn usable_rate(sample_rate: f64) -> f64 {
    if sample_rate.is_finite() && sample_rate > 0.0 {
        sample_rate.clamp(MIN_SAMPLE_RATE, MAX_SAMPLE_RATE)
    } else {
        DEFAULT_SAMPLE_RATE
    }
}

/// The rate every meter assumes until it is prepared, as the rest of the
/// framework does.
pub(crate) const DEFAULT_SAMPLE_RATE: f64 = 44_100.0;

/// The lowest sample rate the meters work at, in hertz. A lower one is
/// taken as this.
///
/// Audio is sampled at rates from 8 kHz to 768 kHz or a little beyond, far
/// inside 1 Hz to 10 MHz. But a rate is only a number in a struct, and far
/// enough outside that range the coefficients derived from it stop being
/// usable: a sample period that overflows, filter poles that round onto
/// the unit circle. The limits keep every meter inside the range its tests
/// cover.
pub(crate) const MIN_SAMPLE_RATE: f64 = 1.0;

/// The highest sample rate the meters work at, in hertz. A higher one is
/// taken as this. See [`MIN_SAMPLE_RATE`].
pub(crate) const MAX_SAMPLE_RATE: f64 = 1e7;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_read_zero_outside_their_channels() {
        let row = Row::new(2);
        row.set(0, 0.25);
        row.set(1, 0.5);
        row.set(7, 9.0);
        assert_eq!(row.len(), 2);
        assert_eq!((row.get(0), row.get(1), row.get(7)), (0.25, 0.5, 0.0));
        assert_eq!(row.max(), 0.5);
        assert_eq!(Row::new(0).max(), 0.0);
    }

    #[test]
    fn a_flag_is_taken_once() {
        let flag = Flag::default();
        assert!(!flag.take());
        flag.raise();
        assert!(flag.take());
        assert!(!flag.take());
    }

    #[test]
    fn a_sample_is_screened_to_a_finite_value_within_the_limit() {
        // Anything a meter can be expected to see passes unchanged, to the
        // bit: the screen must not alter a single reading of good audio.
        for good in [0.0, -0.0, -0.5, 1.0, 7.5e-320, 123_456.789, -INPUT_LIMIT] {
            assert_eq!(screened(good).to_bits(), good.to_bits());
        }
        for (bad, expected) in [
            (f64::NAN, 0.0),
            (f64::INFINITY, 0.0),
            (f64::NEG_INFINITY, 0.0),
            (f64::MAX, INPUT_LIMIT),
            (f64::MIN, -INPUT_LIMIT),
            (1e200, INPUT_LIMIT),
            (-1e200, -INPUT_LIMIT),
            (1.000_000_1e9, INPUT_LIMIT),
            (f64::from(f32::MAX), INPUT_LIMIT),
        ] {
            assert_eq!(screened(bad), expected, "{bad}");
        }
        // What the limit is for: the square of a screened sample, and a sum
        // of a great many of them, are ordinary numbers.
        assert!((INPUT_LIMIT * INPUT_LIMIT * 1e12).is_finite());
        assert!((20.0 * INPUT_LIMIT.log10() - 180.0).abs() < 1e-12);
    }

    #[test]
    fn a_rate_is_made_usable() {
        assert_eq!(usable_rate(0.0), DEFAULT_SAMPLE_RATE);
        assert_eq!(usable_rate(-48_000.0), DEFAULT_SAMPLE_RATE);
        assert_eq!(usable_rate(f64::NAN), DEFAULT_SAMPLE_RATE);
        assert_eq!(usable_rate(f64::INFINITY), DEFAULT_SAMPLE_RATE);
        for rate in [1.0, 50.0, 44_100.0, 96_000.0, 768_000.0, 1e7] {
            assert_eq!(usable_rate(rate), rate);
        }
        // Positive, finite, and of no use to anyone.
        for rate in [5e-324, 1e-300, 0.999] {
            assert_eq!(usable_rate(rate), MIN_SAMPLE_RATE);
        }
        for rate in [1.000_001e7, 1e100, f64::MAX] {
            assert_eq!(usable_rate(rate), MAX_SAMPLE_RATE);
        }
    }

    #[test]
    fn helpers_never_divide_by_nothing() {
        assert_eq!(screened(f64::NAN), 0.0);
        assert_eq!(screened(f64::INFINITY), 0.0);
        assert_eq!(screened(-0.5), -0.5);
        assert_eq!(usable_rate(0.0), DEFAULT_SAMPLE_RATE);
        assert_eq!(usable_rate(f64::NAN), DEFAULT_SAMPLE_RATE);
        assert_eq!(usable_rate(96_000.0), 96_000.0);
        assert_eq!(smoothing_factor(0.0, 48_000.0), 1.0);
        let factor = smoothing_factor(0.3, 48_000.0);
        assert!((factor - 6.944_203e-5).abs() < 1e-10, "{factor}");
        assert_eq!(db(0.0), -100.0);
    }
}
