use core::f64::consts::TAU;

use manguebyte_core::{AudioBuffer, Sample};

use crate::{ProcessSpec, Processor};

/// The shape an [`Oscillator`] produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Waveform {
    /// A pure tone.
    Sine,
    /// A rising ramp, band-limited with PolyBLEP.
    Saw,
    /// A 50% pulse, band-limited with PolyBLEP.
    Square,
    /// A triangle. Not band-limited: its harmonics fall off fast enough that
    /// aliasing only becomes audible at very high pitches.
    Triangle,
}

/// A periodic signal generator with output in `-1.0..=1.0`.
///
/// As a [`Processor`] it overwrites the block, writing the same signal to
/// every channel, which makes it a natural first stage of a chain.
#[derive(Debug, Clone)]
pub struct Oscillator {
    waveform: Waveform,
    frequency: f64,
    sample_rate: f64,
    /// Position in the cycle, `0.0..1.0`.
    phase: f64,
    /// Phase advance per sample.
    increment: f64,
}

impl Oscillator {
    /// Creates an oscillator. Until [`prepare`](Processor::prepare) or
    /// [`set_sample_rate`](Self::set_sample_rate) is called it assumes 44.1 kHz.
    pub fn new(waveform: Waveform, frequency: f64) -> Self {
        let mut oscillator = Self {
            waveform,
            frequency,
            sample_rate: 44_100.0,
            phase: 0.0,
            increment: 0.0,
        };
        oscillator.update_increment();
        oscillator
    }

    /// Changes the frequency in hertz. The phase carries on, so there is no
    /// discontinuity.
    pub fn set_frequency(&mut self, frequency: f64) {
        self.frequency = frequency;
        self.update_increment();
    }

    /// The frequency in hertz.
    pub fn frequency(&self) -> f64 {
        self.frequency
    }

    /// Changes the waveform.
    pub fn set_waveform(&mut self, waveform: Waveform) {
        self.waveform = waveform;
    }

    /// Changes the sample rate in hertz.
    pub fn set_sample_rate(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate;
        self.update_increment();
    }

    /// Restarts the cycle from the beginning.
    pub fn reset_phase(&mut self) {
        self.phase = 0.0;
    }

    /// Produces the next sample.
    #[inline]
    pub fn next_sample(&mut self) -> f64 {
        let (phase, dt) = (self.phase, self.increment);
        let value = match self.waveform {
            Waveform::Sine => (TAU * phase).sin(),
            Waveform::Saw => 2.0 * phase - 1.0 - poly_blep(phase, dt),
            Waveform::Square => {
                let naive = if phase < 0.5 { 1.0 } else { -1.0 };
                naive + poly_blep(phase, dt) - poly_blep((phase + 0.5).fract(), dt)
            }
            Waveform::Triangle => 1.0 - 4.0 * (phase - 0.5).abs(),
        };
        self.phase = (phase + dt).fract();
        value
    }

    fn update_increment(&mut self) {
        // Clamped below Nyquist; PolyBLEP assumes less than half a cycle per sample.
        self.increment = (self.frequency / self.sample_rate).clamp(0.0, 0.5);
    }
}

/// Correction that smooths the step of a naive waveform across the two samples
/// around a discontinuity at `t == 0`, removing most of its aliasing.
#[inline]
fn poly_blep(t: f64, dt: f64) -> f64 {
    if dt <= 0.0 {
        0.0
    } else if t < dt {
        let x = t / dt;
        2.0 * x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + 2.0 * x + 1.0
    } else {
        0.0
    }
}

impl<S: Sample> Processor<S> for Oscillator {
    fn prepare(&mut self, spec: &ProcessSpec) {
        self.set_sample_rate(spec.sample_rate);
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        if buffer.num_channels() == 0 {
            return;
        }
        for sample in buffer.channel_mut(0) {
            *sample = S::from_f64(self.next_sample());
        }
        for index in 1..buffer.num_channels() {
            let (first, other) = buffer.channel_pair_mut(0, index);
            other.copy_from_slice(first);
        }
    }

    fn reset(&mut self) {
        self.reset_phase();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RATE: f64 = 48_000.0;

    fn render(waveform: Waveform, frequency: f64, frames: usize) -> Vec<f64> {
        let mut oscillator = Oscillator::new(waveform, frequency);
        oscillator.set_sample_rate(SAMPLE_RATE);
        (0..frames).map(|_| oscillator.next_sample()).collect()
    }

    #[test]
    fn every_waveform_has_the_requested_frequency() {
        for waveform in [
            Waveform::Sine,
            Waveform::Saw,
            Waveform::Square,
            Waveform::Triangle,
        ] {
            let signal = render(waveform, 100.0, SAMPLE_RATE as usize);
            let rising = signal
                .windows(2)
                .filter(|pair| pair[0] < 0.0 && pair[1] >= 0.0)
                .count();
            assert!(
                (99..=101).contains(&rising),
                "{waveform:?} crossed zero {rising} times"
            );
        }
    }

    #[test]
    fn output_stays_in_range() {
        for waveform in [
            Waveform::Sine,
            Waveform::Saw,
            Waveform::Square,
            Waveform::Triangle,
        ] {
            for frequency in [20.0, 440.0, 9_000.0] {
                let peak = render(waveform, frequency, 4_800)
                    .into_iter()
                    .fold(0.0f64, |peak, s| peak.max(s.abs()));
                assert!(
                    peak <= 1.0 + 1e-9,
                    "{waveform:?} at {frequency} Hz peaked at {peak}"
                );
                // Near Nyquist the band-limited shapes legitimately lose level.
                if frequency < 1_000.0 {
                    assert!(
                        peak > 0.9,
                        "{waveform:?} at {frequency} Hz peaked at {peak}"
                    );
                }
            }
        }
    }

    #[test]
    fn sine_matches_the_reference() {
        let signal = render(Waveform::Sine, 1_000.0, 480);
        for (n, sample) in signal.iter().enumerate() {
            let expected = (TAU * 1_000.0 * n as f64 / SAMPLE_RATE).sin();
            assert!((sample - expected).abs() < 1e-9);
        }
    }

    #[test]
    fn processor_writes_the_same_signal_to_every_channel() {
        let mut oscillator = Oscillator::new(Waveform::Saw, 220.0);
        let mut buffer = AudioBuffer::<f32>::new(3, 128);
        Processor::<f32>::prepare(
            &mut oscillator,
            &ProcessSpec {
                sample_rate: SAMPLE_RATE,
                max_block_size: 128,
                num_channels: 3,
            },
        );
        oscillator.process(&mut buffer);
        assert!(buffer.peak() > 0.5);
        assert_eq!(buffer.channel(0), buffer.channel(1));
        assert_eq!(buffer.channel(0), buffer.channel(2));
    }
}
