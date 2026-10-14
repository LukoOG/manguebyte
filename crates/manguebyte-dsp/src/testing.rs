//! Signals and measurements shared by the unit tests of the filters.

use core::f64::consts::TAU;

use manguebyte_core::{AudioBuffer, Sample};

use crate::Processor;

/// A repeatable source of numbers (xorshift64).
pub(crate) struct Random(u64);

impl Random {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    /// A number from 0 up to but not including 1.
    pub(crate) fn unit(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A number from `low` up to `high`.
    pub(crate) fn between(&mut self, low: f64, high: f64) -> f64 {
        low + (high - low) * self.unit()
    }

    /// A number from `low` up to `high`, spread evenly over its logarithm.
    pub(crate) fn log_between(&mut self, low: f64, high: f64) -> f64 {
        (low.ln() + (high / low).ln() * self.unit()).exp()
    }
}

/// Repeatable noise between -1 and 1, different on each channel.
pub(crate) fn noise<S: Sample>(channels: usize, frames: usize, seed: u64) -> AudioBuffer<S> {
    let mut random = Random::new(seed);
    let mut buffer = AudioBuffer::new(channels, frames);
    for channel in buffer.iter_channels_mut() {
        for sample in channel {
            *sample = S::from_f64(random.between(-1.0, 1.0));
        }
    }
    buffer
}

/// A sine of the given amplitude on every channel.
pub(crate) fn sine<S: Sample>(
    channels: usize,
    frames: usize,
    frequency: f64,
    sample_rate: f64,
    amplitude: f64,
) -> AudioBuffer<S> {
    let mut buffer = AudioBuffer::new(channels, frames);
    for channel in buffer.iter_channels_mut() {
        for (n, sample) in channel.iter_mut().enumerate() {
            *sample = S::from_f64(amplitude * (TAU * frequency * n as f64 / sample_rate).sin());
        }
    }
    buffer
}

/// Runs `signal` through `processor` in blocks of `block` frames.
pub(crate) fn run_in_blocks<S: Sample, P: Processor<S> + ?Sized>(
    processor: &mut P,
    signal: &mut AudioBuffer<S>,
    block: usize,
) {
    let frames = signal.num_frames();
    let mut start = 0;
    while start < frames {
        let end = (start + block.max(1)).min(frames);
        signal.windowed(start..end, |piece| processor.process(piece));
        start = end;
    }
}

/// The response at one frequency, as real and imaginary parts, found by
/// summing an impulse response against a complex sine. There are no bins
/// here, so any frequency can be asked about. The whole response has to be
/// in the slice for the answer to be right.
pub(crate) fn response_at(
    impulse_response: &[f64],
    frequency: f64,
    sample_rate: f64,
) -> (f64, f64) {
    let omega = TAU * frequency / sample_rate;
    let (mut re, mut im) = (0.0, 0.0);
    for (n, &sample) in impulse_response.iter().enumerate() {
        let (sin, cos) = (omega * n as f64).sin_cos();
        re += sample * cos;
        im -= sample * sin;
    }
    (re, im)
}

/// The gain at one frequency, from an impulse response.
pub(crate) fn gain_at(impulse_response: &[f64], frequency: f64, sample_rate: f64) -> f64 {
    let (re, im) = response_at(impulse_response, frequency, sample_rate);
    re.hypot(im)
}

/// A gain in decibels.
pub(crate) fn db(gain: f64) -> f64 {
    20.0 * gain.log10()
}

/// The largest difference between one sample and the next.
pub(crate) fn largest_jump(signal: &[f64]) -> f64 {
    signal
        .windows(2)
        .fold(0.0, |jump, pair| jump.max((pair[1] - pair[0]).abs()))
}

/// The largest sample of the last `len` samples.
pub(crate) fn tail_peak(signal: &[f64], len: usize) -> f64 {
    crate::numeric::strict_peak(&signal[signal.len().saturating_sub(len)..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_measurements_measure() {
        // A delay of three samples at half level: gain 0.5 at any frequency,
        // and a phase of -3ω.
        let mut response = [0.0; 16];
        response[3] = 0.5;
        for frequency in [10.0, 1_000.0, 20_000.0] {
            assert!((gain_at(&response, frequency, 48_000.0) - 0.5).abs() < 1e-15);
            let (re, im) = response_at(&response, frequency, 48_000.0);
            let phase = -3.0 * TAU * frequency / 48_000.0;
            assert!((re - 0.5 * phase.cos()).abs() < 1e-15);
            assert!((im - 0.5 * phase.sin()).abs() < 1e-15);
        }
        assert!((db(0.5) + 6.020_599_913).abs() < 1e-9);
        assert_eq!(largest_jump(&[0.0, 0.25, -0.5, -0.4]), 0.75);
        assert_eq!(tail_peak(&[9.0, 0.5, -0.75], 2), 0.75);

        let mut random = Random::new(7);
        for _ in 0..1_000 {
            let unit = random.unit();
            assert!((0.0..1.0).contains(&unit));
            let wide = random.log_between(5.0, 20_000.0);
            assert!((5.0..=20_000.0).contains(&wide));
        }
        let a = noise::<f64>(2, 64, 3);
        assert!(a == noise::<f64>(2, 64, 3));
        assert!(a.channel(0) != a.channel(1));
        assert!(a.peak() <= 1.0 && a.peak() > 0.5);
        let tone = sine::<f64>(1, 48, 1_000.0, 48_000.0, 0.5);
        assert!((tone.channel(0)[12] - 0.5).abs() < 1e-12);
    }
}
