# Roadmap

Manguebyte aims to become the complete equivalent of JUCE for Rust. That is
years of work, so this document splits it into phases. Each phase leaves the
framework useful for a wider set of projects than the one before.

Nothing here is a promise or a schedule. It is a statement of direction, and it
changes as people show up and build things. If an item interests you, open an
issue and say so.

Legend: ✅ done · 🚧 in progress · ⬜ not started

## Phase 0: foundation (v0.1) ✅

The types everything else is built on, and enough around them to make sound.

- ✅ `manguebyte-core`: `AudioBuffer`, `Sample`, `Smoothed`, atomic floats,
  wait-free SPSC queue, decibel conversions
- ✅ `manguebyte-dsp`: `Processor` trait, compile-time tuple chains,
  band-limited oscillator, biquad filters, ADSR, delay line and echo, gain
- ✅ `manguebyte-midi`: channel voice messages, sample-accurate `MidiBuffer`
- ✅ `manguebyte-formats`: WAV reading and writing
- ✅ `manguebyte-devices`: playback on the default output device
- ✅ `manguebyte`: facade crate and prelude

## Phase 1: a complete audio toolbox

Everything a standalone audio application needs, short of a user interface.

**Devices**
- ✅ Audio input and full-duplex streams
- ✅ Device enumeration and selection; choosing sample rate and buffer size
- ⬜ Hot-plug and default-device change notifications
- ⬜ Clock-drift compensation between duplex devices by adaptive resampling
- ⬜ Choosing the audio host (ASIO, JACK, PipeWire)
- ⬜ MIDI input and output ports

**MIDI**
- ⬜ System common, system real-time and SysEx messages
- ⬜ Running status and a byte-stream parser
- ⬜ Standard MIDI File reading and writing
- ⬜ MPE and MIDI 2.0 / UMP

**DSP**
- ✅ FFT of any length, real FFT and window functions
- ⬜ Short-time Fourier transform with overlap-add; spectrum analysis helpers
- ⬜ Resampling and oversampling
- ✅ State-variable filter, stable under audio-rate modulation
- ⬜ Ladder and FIR filters; higher-order filter design
- ✅ Dynamics: compressor with soft knee, noise gate
- ⬜ Look-ahead limiter, expander, sidechain input
- ✅ Reverb (feedback delay network)
- ⬜ Chorus, phaser, convolution
- ⬜ Wavetable oscillator, noise generators, LFO
- ⬜ Polyphonic voice allocation
- ⬜ SIMD-accelerated buffer operations

**Formats**
- ⬜ Streaming readers and writers that do not load whole files
- ⬜ AIFF, FLAC, Ogg Vorbis, Opus and MP3 decoding
- ⬜ FLAC and Ogg Vorbis encoding
- ⬜ Metadata, cue points and loop markers

**Core**
- ✅ Parameter system: ranges, skew, text conversion, smoothing
- ⬜ Sample-accurate parameter automation events and change gestures
- ✅ Parameter state save and restore, tolerant of version changes
- ⬜ Audio thread helpers: denormal protection, allocation checking in debug
- ⬜ `no_std` support for `manguebyte-core` and `manguebyte-dsp`

## Phase 2: plugins

Write a processor once and ship it in every format.

- ⬜ `manguebyte-plugin`: the format-independent plugin API
- ⬜ CLAP
- ⬜ VST3
- ⬜ Audio Unit (AUv2 and AUv3)
- ⬜ LV2
- ⬜ Standalone application wrapper
- ⬜ `cargo` tooling to bundle, sign and validate plugins
- ⬜ Project templates

AAX depends on a proprietary SDK and will be considered once the other formats
are solid.

## Phase 3: user interface

- ⬜ GPU-accelerated 2D renderer
- ⬜ Windowing, including embedding inside a plugin host
- ⬜ Layout, styling and a widget set: sliders, knobs, buttons, meters, text
- ⬜ Audio widgets: waveform, spectrum, keyboard, envelope and EQ editors
- ⬜ Accessibility
- ⬜ High-DPI and multi-monitor support

## Phase 4: hosting and graphs

- ⬜ Audio processing graph with automatic latency compensation
- ⬜ Plugin scanning and hosting: CLAP, VST3, AU, LV2
- ⬜ Transport, tempo map and timeline primitives

## Phase 5: everywhere

- ⬜ iOS and Android
- ⬜ WebAssembly and Web Audio
- ⬜ Embedded targets

## Beyond parity

Matching JUCE is the baseline, not the finish line. Places where a framework
designed for Rust can do better:

- Compile-time checked real-time safety for user code
- Processing graphs that the compiler can inline end to end
- Deterministic offline rendering for testing audio code in CI
- First-class CLAP support, including its newer extensions
- A plugin API in which sharing state between threads incorrectly does not
  compile
