use manguebyte_core::{AudioBuffer, Sample};

use crate::{ProcessSpec, Processor};

/// A circular buffer that reads back what was written some samples ago, with
/// linear interpolation for fractional delays.
///
/// This is the primitive behind echoes, choruses, flangers and comb filters.
#[derive(Debug, Clone)]
pub struct DelayLine<S: Sample = f32> {
    buffer: Vec<S>,
    mask: usize,
    /// Slot the next [`push`](Self::push) writes to.
    write: usize,
    max_delay: usize,
}

impl<S: Sample> DelayLine<S> {
    /// Creates a silent line able to delay by up to `max_delay` samples.
    /// This allocates.
    pub fn new(max_delay: usize) -> Self {
        // One slot for the newest sample and one for interpolation headroom.
        let len = (max_delay + 2).next_power_of_two();
        Self {
            buffer: vec![S::ZERO; len],
            mask: len - 1,
            write: 0,
            max_delay,
        }
    }

    /// Longest delay, in samples, that [`read`](Self::read) can serve.
    pub fn max_delay(&self) -> usize {
        self.max_delay
    }

    /// Writes one sample.
    #[inline]
    pub fn push(&mut self, sample: S) {
        self.buffer[self.write] = sample;
        self.write = (self.write + 1) & self.mask;
    }

    /// Reads the sample written `delay` pushes ago: `1.0` is the most recent.
    ///
    /// `delay` is clamped to `1.0..=max_delay`.
    #[inline]
    pub fn read(&self, delay: f64) -> S {
        let delay = delay.clamp(1.0, self.max_delay.max(1) as f64);
        let whole = delay as usize;
        let fraction = S::from_f64(delay - whole as f64);
        let newer = self.buffer[self.write.wrapping_sub(whole) & self.mask];
        let older = self.buffer[self.write.wrapping_sub(whole + 1) & self.mask];
        newer + (older - newer) * fraction
    }

    /// Silences the line.
    pub fn clear(&mut self) {
        self.buffer.fill(S::ZERO);
    }
}

/// A feedback delay (echo) effect.
///
/// Changing the delay time while audio is running jumps the read position,
/// which can click; smooth it yourself if you need to sweep it.
#[derive(Debug, Clone)]
pub struct Delay<S: Sample = f32> {
    lines: Vec<DelayLine<S>>,
    max_seconds: f64,
    time_seconds: f64,
    sample_rate: f64,
    feedback: S,
    mix: S,
}

impl<S: Sample> Delay<S> {
    /// Creates an echo that can delay by up to `max_seconds`, initially set
    /// to that maximum with no feedback and an equal dry/wet mix.
    pub fn new(max_seconds: f64) -> Self {
        Self {
            lines: Vec::new(),
            max_seconds,
            time_seconds: max_seconds,
            sample_rate: 44_100.0,
            feedback: S::ZERO,
            mix: S::from_f64(0.5),
        }
    }

    /// Sets the delay time in seconds, clamped to the maximum.
    pub fn set_time(&mut self, seconds: f64) {
        self.time_seconds = seconds.clamp(0.0, self.max_seconds);
    }

    /// Sets how much of the delayed signal is fed back in, clamped to
    /// `0.0..=0.99` so the echoes always die away.
    pub fn set_feedback(&mut self, feedback: f64) {
        self.feedback = S::from_f64(feedback.clamp(0.0, 0.99));
    }

    /// Sets the dry/wet balance: `0.0` is all dry, `1.0` all delayed.
    pub fn set_mix(&mut self, mix: f64) {
        self.mix = S::from_f64(mix.clamp(0.0, 1.0));
    }

    /// Builder form of [`set_time`](Self::set_time).
    pub fn with_time(mut self, seconds: f64) -> Self {
        self.set_time(seconds);
        self
    }

    /// Builder form of [`set_feedback`](Self::set_feedback).
    pub fn with_feedback(mut self, feedback: f64) -> Self {
        self.set_feedback(feedback);
        self
    }

    /// Builder form of [`set_mix`](Self::set_mix).
    pub fn with_mix(mut self, mix: f64) -> Self {
        self.set_mix(mix);
        self
    }
}

impl<S: Sample> Processor<S> for Delay<S> {
    fn prepare(&mut self, spec: &ProcessSpec) {
        self.sample_rate = spec.sample_rate;
        let max_delay = (self.max_seconds * spec.sample_rate).ceil().max(1.0) as usize;
        self.lines = (0..spec.num_channels)
            .map(|_| DelayLine::new(max_delay))
            .collect();
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        debug_assert!(
            buffer.num_channels() <= self.lines.len(),
            "buffer has more channels than the delay was prepared for"
        );
        let delay = self.time_seconds * self.sample_rate;
        let (feedback, mix) = (self.feedback, self.mix);
        let dry = S::ONE - mix;
        for (channel, line) in buffer.iter_channels_mut().zip(&mut self.lines) {
            for sample in channel {
                let input = *sample;
                let delayed = line.read(delay);
                line.push(input + delayed * feedback);
                *sample = input * dry + delayed * mix;
            }
        }
    }

    fn reset(&mut self) {
        for line in &mut self.lines {
            line.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_reads_back_by_whole_samples() {
        let mut line = DelayLine::<f32>::new(8);
        for value in 1..=5 {
            line.push(value as f32);
        }
        assert_eq!(line.read(1.0), 5.0);
        assert_eq!(line.read(3.0), 3.0);
        assert_eq!(line.read(5.0), 1.0);
        assert_eq!(line.read(8.0), 0.0);
    }

    #[test]
    fn line_interpolates_and_clamps() {
        let mut line = DelayLine::<f64>::new(4);
        line.push(10.0);
        line.push(20.0);
        assert_eq!(line.read(1.5), 15.0);
        assert_eq!(line.read(0.0), 20.0);
        assert_eq!(line.read(100.0), line.read(4.0));
    }

    #[test]
    fn line_survives_wrapping() {
        let mut line = DelayLine::<f32>::new(3);
        for value in 0..1_000 {
            line.push(value as f32);
            assert_eq!(line.read(1.0), value as f32);
            if value >= 2 {
                assert_eq!(line.read(3.0), (value - 2) as f32);
            }
        }
    }

    #[test]
    fn echoes_arrive_on_time_and_decay() {
        // 10 samples of delay at 1 kHz.
        let mut delay = Delay::<f64>::new(0.05)
            .with_time(0.01)
            .with_feedback(0.5)
            .with_mix(1.0);
        delay.prepare(&ProcessSpec {
            sample_rate: 1_000.0,
            max_block_size: 32,
            num_channels: 1,
        });

        let mut buffer = AudioBuffer::<f64>::new(1, 32);
        buffer.channel_mut(0)[0] = 1.0;
        delay.process(&mut buffer);

        let out = buffer.channel(0);
        for (frame, &sample) in out.iter().enumerate() {
            let expected = match frame {
                10 => 1.0,
                20 => 0.5,
                30 => 0.25,
                _ => 0.0,
            };
            assert_eq!(sample, expected, "frame {frame}");
        }
    }

    #[test]
    fn dry_mix_leaves_the_signal_alone_and_reset_clears_the_tail() {
        let spec = ProcessSpec {
            sample_rate: 1_000.0,
            max_block_size: 16,
            num_channels: 2,
        };
        let mut delay = Delay::<f32>::new(0.004).with_mix(0.0);
        delay.prepare(&spec);
        let mut buffer = AudioBuffer::<f32>::new(2, 16);
        buffer.channel_mut(0).fill(0.5);
        delay.process(&mut buffer);
        assert_eq!(buffer.channel(0), &[0.5; 16]);

        delay.set_mix(1.0);
        delay.reset();
        buffer.clear();
        delay.process(&mut buffer);
        assert_eq!(buffer.peak(), 0.0);
    }
}
