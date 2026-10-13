//! Manguebyte is a real-time-safe audio framework for Rust.
//!
//! This crate gathers the whole framework under one name. Each area lives in
//! its own crate, which you can also depend on directly:
//!
//! | Module | Crate | What it holds |
//! |---|---|---|
//! | (root) | `manguebyte-core` | [`AudioBuffer`], [`Sample`], [`Smoothed`], atomics, [`spsc`] queue |
//! | [`dsp`] | `manguebyte-dsp` | processor chains, sample-accurate events, polyphonic synth, filters, effects, FFT |
//! | [`params`] | `manguebyte-params` | thread-safe parameters, ranges, smoothing, saved state |
//! | [`graph`] | `manguebyte-graph` | a processing graph that can be rewired while it plays |
//! | [`plugin`] | `manguebyte-plugin` | the format-independent plugin API and a test harness |
//! | [`midi`] | `manguebyte-midi` | MIDI messages, stream parsing, MIDI files, sample-accurate event buffers |
//! | [`formats`] | `manguebyte-formats` | audio file reading and writing |
//! | `clap` | `manguebyte-clap` | builds CLAP plugins (feature `clap`) |
//! | `standalone` | `manguebyte-standalone` | runs a plugin on the sound card and MIDI ports (feature `standalone`) |
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

pub use manguebyte_core::{
    AtomicF32, AtomicF64, AudioBuffer, ParamId, Sample, Smoothed, decibels, spsc,
};

#[cfg(feature = "clap")]
pub use manguebyte_clap as clap;
#[cfg(feature = "devices")]
pub use manguebyte_devices as devices;
pub use manguebyte_dsp as dsp;
pub use manguebyte_formats as formats;
pub use manguebyte_graph as graph;
pub use manguebyte_midi as midi;
pub use manguebyte_params as params;
pub use manguebyte_plugin as plugin;
#[cfg(feature = "standalone")]
pub use manguebyte_standalone as standalone;

/// The names most programs need, in one import.
pub mod prelude {
    pub use crate::dsp::event::{Event, EventBuffer, EventKind, Expression, NoteRef};
    pub use crate::dsp::{
        Adsr, AdsrParams, Biquad, BiquadCoefficients, Compressor, DcBlocker, Delay, DelayLine,
        Filter, FilterShape, Gain, Lfo, LfoShape, Limiter, Noise, NoiseColor, NoiseGate,
        Oscillator, Oversampled, ProcessContext, ProcessSpec, Processor, Resampler, Reverb,
        ShaperCurve, StateVariableFilter, SvfMode, Synth, Transport, Voice, Waveform, Waveshaper,
        resample, split_at_events,
    };
    pub use crate::formats::wav::{self, WavFormat};
    pub use crate::graph::{Graph, GraphEngine, GraphError, NodeId};
    pub use crate::midi::file::{MetaEvent, MidiFile, Track};
    pub use crate::midi::{
        MidiBuffer, MidiEvent, MidiMessage, StreamEvent, StreamParser, SystemMessage, hz_to_note,
        note_to_hz,
    };
    pub use crate::params::{
        BoolParam, Changed, ChoiceParam, FloatParam, FloatRange, IntParam, Param, ParamSet,
        ParamSmoother, ParamState, Smoothing, param_set,
    };
    pub use crate::plugin::{AudioLayout, Harness, Plugin, PluginInfo, PluginKind};
    pub use crate::{AtomicF32, AtomicF64, AudioBuffer, ParamId, Sample, Smoothed, decibels, spsc};

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
