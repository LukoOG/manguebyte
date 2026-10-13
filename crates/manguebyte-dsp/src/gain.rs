use manguebyte_core::{AudioBuffer, Sample, Smoothed, decibels};

use crate::{ProcessSpec, Processor};

/// Scales the signal, gliding to new settings so that changes never click.
#[derive(Debug, Clone)]
pub struct Gain<S: Sample = f32> {
    gain: Smoothed<S>,
    ramp_seconds: f64,
}

impl<S: Sample> Gain<S> {
    /// Default glide time for gain changes.
    pub const DEFAULT_RAMP_SECONDS: f64 = 0.02;

    /// Creates a gain stage with a linear factor.
    pub fn new(gain: S) -> Self {
        Self {
            gain: Smoothed::new(gain),
            ramp_seconds: Self::DEFAULT_RAMP_SECONDS,
        }
    }

    /// Creates a gain stage from a level in decibels.
    pub fn from_db(db: f64) -> Self {
        Self::new(S::from_f64(decibels::db_to_gain(db)))
    }

    /// Sets how long a gain change takes. Applies from the next
    /// [`prepare`](Processor::prepare).
    pub fn with_ramp(mut self, seconds: f64) -> Self {
        self.ramp_seconds = seconds;
        self
    }

    /// Glides to a new linear factor.
    pub fn set_gain(&mut self, gain: S) {
        self.gain.set_target(gain);
    }

    /// Glides to a new level in decibels.
    pub fn set_gain_db(&mut self, db: f64) {
        self.set_gain(S::from_f64(decibels::db_to_gain(db)));
    }

    /// The linear factor being approached.
    pub fn gain(&self) -> S {
        self.gain.target()
    }
}

impl<S: Sample> Processor<S> for Gain<S> {
    fn prepare(&mut self, spec: &ProcessSpec) {
        self.gain.set_ramp(spec.sample_rate, self.ramp_seconds);
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        if !self.gain.is_smoothing() {
            buffer.apply_gain(self.gain.target());
            return;
        }
        // Every channel must see the same ramp, so each one runs a copy and
        // the original is advanced once at the end.
        for channel in buffer.iter_channels_mut() {
            let mut gain = self.gain;
            for sample in channel {
                *sample *= gain.next_value();
            }
        }
        self.gain.skip(buffer.num_frames());
    }

    fn reset(&mut self) {
        self.gain.set_immediate(self.gain.target());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> ProcessSpec {
        ProcessSpec {
            sample_rate: 1000.0,
            max_block_size: 16,
            num_channels: 2,
        }
    }

    fn ones() -> AudioBuffer<f32> {
        let mut buffer = AudioBuffer::new(2, 16);
        for channel in buffer.iter_channels_mut() {
            channel.fill(1.0);
        }
        buffer
    }

    #[test]
    fn applies_a_steady_gain() {
        let mut gain = Gain::<f32>::from_db(-6.0);
        gain.prepare(&spec());
        let mut buffer = ones();
        gain.process(&mut buffer);
        assert!((buffer.peak() - 0.501_187).abs() < 1e-5);
    }

    #[test]
    fn ramps_identically_on_every_channel() {
        // 10 ms at 1 kHz is a 10-sample ramp.
        let mut gain = Gain::new(0.0f32).with_ramp(0.01);
        gain.prepare(&spec());
        gain.set_gain(1.0);

        let mut buffer = ones();
        gain.process(&mut buffer);

        assert_eq!(buffer.channel(0), buffer.channel(1));
        let left = buffer.channel(0);
        assert!((left[0] - 0.1).abs() < 1e-6);
        assert!(left.windows(2).all(|pair| pair[1] >= pair[0]));
        assert_eq!(&left[9..], &[1.0; 7]);

        let mut next = ones();
        gain.process(&mut next);
        assert_eq!(next.channel(0), &[1.0; 16]);
    }
}
