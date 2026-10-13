//! Signal processing building blocks.
//!
//! Everything here implements [`Processor`], and a tuple of processors is
//! itself a processor that runs its members in order. The chain is resolved at
//! compile time, so it costs nothing over calling each stage by hand:
//!
//! ```
//! use manguebyte_core::AudioBuffer;
//! use manguebyte_dsp::{Filter, FilterShape, Gain, Oscillator, ProcessSpec, Processor, Waveform};
//!
//! let mut voice = (
//!     Oscillator::new(Waveform::Saw, 110.0),
//!     Filter::<f32>::new(FilterShape::Lowpass, 800.0, 0.707),
//!     Gain::from_db(-12.0),
//! );
//!
//! let spec = ProcessSpec { sample_rate: 48_000.0, max_block_size: 256, num_channels: 2 };
//! voice.prepare(&spec);
//!
//! let mut block = AudioBuffer::<f32>::new(2, 256);
//! voice.process(&mut block);
//! assert!(block.peak() > 0.0);
//! ```

mod biquad;
mod delay;
mod envelope;
pub mod fft;
mod gain;
mod oscillator;
pub mod window;

pub use biquad::{Biquad, BiquadCoefficients, Filter, FilterShape};
pub use delay::{Delay, DelayLine};
pub use envelope::{Adsr, AdsrParams};
pub use gain::Gain;
pub use oscillator::{Oscillator, Waveform};

use manguebyte_core::{AudioBuffer, Sample};

/// The conditions a processor will run under.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProcessSpec {
    /// Sample rate in hertz.
    pub sample_rate: f64,
    /// Largest block [`Processor::process`] will be handed.
    pub max_block_size: usize,
    /// Number of channels in the blocks to come.
    pub num_channels: usize,
}

/// A unit that transforms audio in place, one block at a time.
///
/// # Real-time contract
///
/// [`prepare`](Self::prepare) may allocate and take its time.
/// [`process`](Self::process) and [`reset`](Self::reset) run on the audio
/// thread: they must not allocate, lock, or do I/O.
pub trait Processor<S: Sample = f32> {
    /// Gets ready to process under `spec`. Called before the first block and
    /// again whenever the sample rate, block size or channel count changes.
    fn prepare(&mut self, spec: &ProcessSpec);

    /// Processes one block in place.
    fn process(&mut self, buffer: &mut AudioBuffer<S>);

    /// Clears internal state such as filter memory and delay tails, without
    /// touching parameters.
    fn reset(&mut self) {}
}

impl<S: Sample, P: Processor<S> + ?Sized> Processor<S> for Box<P> {
    fn prepare(&mut self, spec: &ProcessSpec) {
        (**self).prepare(spec);
    }
    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        (**self).process(buffer);
    }
    fn reset(&mut self) {
        (**self).reset();
    }
}

macro_rules! impl_processor_for_tuple {
    ($($member:ident),+) => {
        #[allow(non_snake_case)]
        impl<S: Sample, $($member: Processor<S>),+> Processor<S> for ($($member,)+) {
            fn prepare(&mut self, spec: &ProcessSpec) {
                let ($($member,)+) = self;
                $($member.prepare(spec);)+
            }
            fn process(&mut self, buffer: &mut AudioBuffer<S>) {
                let ($($member,)+) = self;
                $($member.process(buffer);)+
            }
            fn reset(&mut self) {
                let ($($member,)+) = self;
                $($member.reset();)+
            }
        }
    };
}

impl_processor_for_tuple!(A);
impl_processor_for_tuple!(A, B);
impl_processor_for_tuple!(A, B, C);
impl_processor_for_tuple!(A, B, C, D);
impl_processor_for_tuple!(A, B, C, D, E);
impl_processor_for_tuple!(A, B, C, D, E, F);
impl_processor_for_tuple!(A, B, C, D, E, F, G);
impl_processor_for_tuple!(A, B, C, D, E, F, G, H);

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: ProcessSpec = ProcessSpec {
        sample_rate: 48_000.0,
        max_block_size: 64,
        num_channels: 1,
    };

    #[test]
    fn tuple_runs_members_in_order() {
        struct Add(f32);
        struct Mul(f32);
        impl Processor for Add {
            fn prepare(&mut self, _: &ProcessSpec) {}
            fn process(&mut self, buffer: &mut AudioBuffer) {
                buffer.channel_mut(0).iter_mut().for_each(|s| *s += self.0);
            }
        }
        impl Processor for Mul {
            fn prepare(&mut self, _: &ProcessSpec) {}
            fn process(&mut self, buffer: &mut AudioBuffer) {
                buffer.apply_gain(self.0);
            }
        }

        let mut chain = (Add(1.0), Mul(3.0), Add(0.5));
        chain.prepare(&SPEC);
        let mut buffer = AudioBuffer::new(1, 4);
        chain.process(&mut buffer);
        assert_eq!(buffer.channel(0), &[3.5; 4]);
    }

    #[test]
    fn boxed_processors_can_be_chained_dynamically() {
        let mut chain: Vec<Box<dyn Processor>> = vec![
            Box::new(Oscillator::new(Waveform::Sine, 440.0)),
            Box::new(Gain::new(0.5)),
        ];
        let mut buffer = AudioBuffer::new(1, 64);
        for stage in &mut chain {
            stage.prepare(&SPEC);
        }
        for stage in &mut chain {
            stage.process(&mut buffer);
        }
        let peak = buffer.peak();
        assert!(peak > 0.2 && peak <= 0.5, "peak was {peak}");
    }
}
