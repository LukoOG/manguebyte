//! Designing finite impulse response filters.
//!
//! These functions return coefficients; they do not process audio
//! themselves. They are what the resampler and the oversampler are built
//! from, and are public for anyone who needs a linear-phase filter.
//!
//! The method is the windowed sinc with a Kaiser window, as in J. F. Kaiser,
//! "Nonrecursive digital filter design using the I0-sinh window function",
//! Proc. IEEE International Symposium on Circuits and Systems, 1974.

use core::f64::consts::PI;

use crate::window::{Symmetry, Window};

/// `sin(πx) / (πx)`, with the limit at zero filled in.
pub fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        (PI * x).sin() / (PI * x)
    }
}

/// The Kaiser window parameter that gives a stopband this many decibels
/// down. Kaiser's empirical formula.
pub fn kaiser_beta(attenuation_db: f64) -> f64 {
    if attenuation_db > 50.0 {
        0.1102 * (attenuation_db - 8.7)
    } else if attenuation_db >= 21.0 {
        0.5842 * (attenuation_db - 21.0).powf(0.4) + 0.07886 * (attenuation_db - 21.0)
    } else {
        0.0
    }
}

/// How many taps a Kaiser-windowed filter needs for a given stopband
/// attenuation and transition width. The width is a fraction of the sample
/// rate. The result is always odd, so the filter has a whole-sample delay.
pub fn kaiser_taps(attenuation_db: f64, transition_width: f64) -> usize {
    let taps = ((attenuation_db - 7.95) / (14.36 * transition_width)).ceil() as usize + 1;
    taps | 1
}

/// A linear-phase low-pass filter.
///
/// `cutoff` is the middle of the transition band as a fraction of the sample
/// rate, between 0 and 0.5. The coefficients are scaled so that a constant
/// signal passes unchanged, and the filter delays by `(taps - 1) / 2`
/// samples.
///
/// # Panics
///
/// Panics if `taps` is zero or `cutoff` is outside `0..=0.5`.
pub fn lowpass(taps: usize, cutoff: f64, attenuation_db: f64) -> Vec<f64> {
    assert!(taps > 0, "a filter needs at least one tap");
    assert!(
        (0.0..=0.5).contains(&cutoff),
        "cutoff must be a fraction of the sample rate between 0 and 0.5"
    );
    let window = Window::Kaiser(kaiser_beta(attenuation_db));
    let centre = (taps - 1) as f64 / 2.0;
    let mut coefficients: Vec<f64> = (0..taps)
        .map(|index| {
            let offset = index as f64 - centre;
            2.0 * cutoff
                * sinc(2.0 * cutoff * offset)
                * window.value(index, taps, Symmetry::Symmetric)
        })
        .collect();
    let sum: f64 = coefficients.iter().sum();
    for coefficient in &mut coefficients {
        *coefficient /= sum;
    }
    coefficients
}

/// The gain of a filter at a frequency given as a fraction of the sample
/// rate.
pub fn magnitude_at(coefficients: &[f64], frequency: f64) -> f64 {
    let (mut re, mut im) = (0.0, 0.0);
    for (index, &coefficient) in coefficients.iter().enumerate() {
        let phase = -2.0 * PI * frequency * index as f64;
        re += coefficient * phase.cos();
        im += coefficient * phase.sin();
    }
    re.hypot(im)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db(gain: f64) -> f64 {
        20.0 * gain.max(1e-300).log10()
    }

    #[test]
    fn lowpass_meets_its_specification() {
        // 80 dB down, transition from 0.20 to 0.30 of the sample rate.
        let taps = kaiser_taps(80.0, 0.10);
        let filter = lowpass(taps, 0.25, 80.0);
        assert_eq!(taps % 2, 1);
        assert!((filter.iter().sum::<f64>() - 1.0).abs() < 1e-12);

        for step in 0..=200 {
            let frequency = 0.20 * f64::from(step) / 200.0;
            assert!(
                db(magnitude_at(&filter, frequency)).abs() < 0.01,
                "{frequency}"
            );
        }
        for step in 0..=200 {
            let frequency = 0.30 + 0.20 * f64::from(step) / 200.0;
            assert!(db(magnitude_at(&filter, frequency)) < -78.0, "{frequency}");
        }
        // Halfway through the transition it is 6 dB down.
        assert!((db(magnitude_at(&filter, 0.25)) + 6.02).abs() < 0.1);
    }

    #[test]
    fn coefficients_are_symmetric_so_the_phase_is_linear() {
        let filter = lowpass(31, 0.1, 60.0);
        for (a, b) in filter.iter().zip(filter.iter().rev()) {
            assert!((a - b).abs() < 1e-15);
        }
    }

    #[test]
    fn kaiser_formulas_match_published_values() {
        // Oppenheim and Schafer, Discrete-Time Signal Processing, example
        // of a 60 dB design.
        assert!((kaiser_beta(60.0) - 5.653).abs() < 0.001);
        assert_eq!(kaiser_beta(10.0), 0.0);
        assert!(kaiser_taps(60.0, 0.1) >= 37);
        assert!((sinc(0.0) - 1.0).abs() < 1e-15 && sinc(1.0).abs() < 1e-15);
    }
}
