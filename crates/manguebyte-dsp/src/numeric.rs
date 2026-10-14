//! Small numeric helpers shared by the processors.

use manguebyte_core::Smoothed;
#[cfg(test)]
use manguebyte_core::{AudioBuffer, Sample};

/// Limits `value` to `low..=high`.
///
/// Unlike `f64::clamp` this never panics, whatever the bounds are, and it
/// turns not-a-number into `low`. A setter that is handed garbage therefore
/// still leaves its processor in a valid state, and the audio thread can use
/// it on values computed from the signal.
#[inline]
pub(crate) fn limit(value: f64, low: f64, high: f64) -> f64 {
    if value >= low { value.min(high) } else { low }
}

/// Starts a glide towards `target`, unless the value is already heading
/// there: setting the same target again would start the ramp again, and a
/// setter that is called on every block would then never arrive.
#[inline]
pub(crate) fn glide_to(value: &mut Smoothed<f64>, target: f64) {
    if value.target() != target {
        value.set_target(target);
    }
}

/// Moves `value` towards `target` by at most `step`.
#[inline]
pub(crate) fn step_towards(value: f64, target: f64, step: f64) -> f64 {
    if value < target {
        (value + step).min(target)
    } else {
        (value - step).max(target)
    }
}

/// The largest sample in `samples`, with anything that is not a finite
/// number counted as infinite. For tests.
///
/// `AudioBuffer::peak` reports a NaN as NaN, which every comparison treats
/// as false; a test that has to show that an output is finite, bounded or
/// silent can use this instead and compare with plain numbers.
#[cfg(test)]
pub(crate) fn strict_peak<S: Sample>(samples: &[S]) -> f64 {
    samples.iter().fold(0.0, |peak: f64, sample| {
        let size = sample.to_f64().abs();
        if size.is_finite() {
            peak.max(size)
        } else {
            f64::INFINITY
        }
    })
}

/// [`strict_peak`] over every channel of a buffer. For tests.
#[cfg(test)]
pub(crate) fn strict_buffer_peak<S: Sample>(buffer: &AudioBuffer<S>) -> f64 {
    buffer.iter_channels().map(strict_peak).fold(0.0, f64::max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_strict_peak_counts_anything_not_finite_as_infinite() {
        let mut buffer = AudioBuffer::<f32>::new(2, 8);
        buffer.channel_mut(0)[3] = -0.5;
        buffer.channel_mut(1)[5] = 0.25;
        assert_eq!(strict_buffer_peak(&buffer), 0.5);
        assert_eq!(strict_peak(buffer.channel(1)), 0.25);

        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut broken = AudioBuffer::<f32>::new(2, 8);
            broken.channel_mut(1).fill(bad);
            assert_eq!(strict_buffer_peak(&broken), f64::INFINITY);
        }
        let mut broken = AudioBuffer::<f32>::new(1, 8);
        broken.channel_mut(0).fill(f32::NAN);
        // The plain peak does not hide it either.
        assert!(broken.peak().is_nan());
        assert_eq!(strict_peak::<f64>(&[]), 0.0);
    }

    #[test]
    fn limits_and_survives_garbage() {
        assert_eq!(limit(0.5, 0.0, 1.0), 0.5);
        assert_eq!(limit(-3.0, 0.0, 1.0), 0.0);
        assert_eq!(limit(3.0, 0.0, 1.0), 1.0);
        assert_eq!(limit(f64::NAN, 0.25, 1.0), 0.25);
        assert_eq!(limit(f64::INFINITY, 0.0, 1.0), 1.0);
        assert_eq!(limit(f64::NEG_INFINITY, 0.0, 1.0), 0.0);
        // Bounds in the wrong order do not panic.
        assert_eq!(limit(5.0, 2.0, 1.0), 1.0);
        assert_eq!(limit(0.0, 2.0, 1.0), 2.0);
    }

    #[test]
    fn a_repeated_target_does_not_restart_a_glide() {
        let mut value = Smoothed::new(0.0f64);
        value.set_ramp(4.0, 1.0);
        glide_to(&mut value, 1.0);
        assert_eq!(value.next_value(), 0.25);
        glide_to(&mut value, 1.0);
        assert_eq!(value.next_value(), 0.5);
        assert_eq!(value.next_value(), 0.75);
        assert_eq!(value.next_value(), 1.0);
    }

    #[test]
    fn steps_arrive_exactly_and_do_not_overshoot() {
        assert_eq!(step_towards(0.0, 1.0, 0.25), 0.25);
        assert_eq!(step_towards(0.9, 1.0, 0.25), 1.0);
        assert_eq!(step_towards(1.0, 0.0, 0.25), 0.75);
        assert_eq!(step_towards(0.1, 0.0, 0.25), 0.0);
        assert_eq!(step_towards(0.5, 0.5, 0.25), 0.5);
    }
}
