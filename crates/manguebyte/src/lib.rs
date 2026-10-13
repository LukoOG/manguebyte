//! Manguebyte is a real-time-safe audio framework for Rust.
//!
//! This crate gathers the whole framework under one name. Each area lives in
//! its own crate, which you can also depend on directly:
//!
//! | Module | Crate | What it holds |
//! |---|---|---|
//! | (root) | `manguebyte-core` | [`AudioBuffer`], [`Sample`], [`Smoothed`], atomics, [`spsc`] queue |
//! | [`dsp`] | `manguebyte-dsp` | oscillators, filters, envelopes, delays, processor chains |
//! | [`midi`] | `manguebyte-midi` | MIDI messages and sample-accurate event buffers |
//! | [`formats`] | `manguebyte-formats` | audio file reading and writing |
//! | [`devices`] | `manguebyte-devices` | audio device input, output and duplex streams (feature `devices`, on by default) |
//!
//! # Example
//!
//! Render a filtered sawtooth and save it as a WAV file:
//!
//! ```
//! use manguebyte::prelude::*;
//!
//! let spec = ProcessSpec { sample_rate: 48_000.0, max_block_size: 480, num_channels: 2 };
//! let mut voice = (
//!     Oscillator::new(Waveform::Saw, note_to_hz(45.0)),
//!     Filter::new(FilterShape::Lowpass, 1_200.0, 1.5),
//!     Gain::from_db(-12.0),
//! );
//! voice.prepare(&spec);
//!
//! let mut block = AudioBuffer::<f32>::new(2, 480);
//! voice.process(&mut block);
//!
//! let mut file = Vec::new();
//! wav::write(&mut file, &block, 48_000, WavFormat::Int16)?;
//! # Ok::<(), wav::WavError>(())
//! ```

pub use manguebyte_core::{AtomicF32, AtomicF64, AudioBuffer, Sample, Smoothed, decibels, spsc};

#[cfg(feature = "devices")]
pub use manguebyte_devices as devices;
pub use manguebyte_dsp as dsp;
pub use manguebyte_formats as formats;
pub use manguebyte_midi as midi;

/// The names most programs need, in one import.
pub mod prelude {
    pub use crate::dsp::{
        Adsr, AdsrParams, Biquad, BiquadCoefficients, Delay, DelayLine, Filter, FilterShape, Gain,
        Oscillator, ProcessSpec, Processor, Waveform,
    };
    pub use crate::formats::wav::{self, WavFormat};
    pub use crate::midi::{MidiBuffer, MidiEvent, MidiMessage, hz_to_note, note_to_hz};
    pub use crate::{AtomicF32, AtomicF64, AudioBuffer, Sample, Smoothed, decibels, spsc};

    #[cfg(feature = "devices")]
    pub use crate::devices::{
        DeviceError, DeviceInfo, DuplexStream, InputDevice, InputStream, OutputDevice,
        OutputStream, StreamSettings, input_devices, output_devices,
    };
}

/// Compiles the code blocks of the README as tests, so they cannot go stale.
#[cfg(all(doctest, feature = "devices"))]
#[doc = include_str!("../../../README.md")]
struct ReadmeDoctests;
