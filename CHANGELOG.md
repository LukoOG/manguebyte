# Changelog

What changed in Manguebyte, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). Before 1.0, a minor version may
break the API.

## Unreleased

Nothing has been published to crates.io yet. Everything below is on `main`.

### Plugins

- `manguebyte-clap`: native CLAP support. `export_clap!(MyPlugin)` turns a
  `Plugin` into a CLAP plugin with audio and note ports, sample-accurate
  events, parameters, state and latency. The example plugin passes the CLAP
  project's validator, which runs in CI.
- `manguebyte-vst3`: native VST 3 support. `export_vst3!(MyPlugin)` turns
  the same `Plugin` into a VST 3 plugin: audio, sidechain and event buses;
  every point of every automation queue on its frame; notes with ids,
  tuning, polyphonic pressure and note expression; MIDI controllers, channel
  pressure and pitch bend through hidden parameters; units; state; latency.
  The class id is derived from the plugin's text id and is the same on every
  platform. The example plugin passes all 537 tests of Steinberg's validator
  in its extensive mode, which runs in CI on Windows, Linux and macOS, and
  the wrapper's own tests run under Miri.
- `scripts/bundle-vst3.sh` builds a `.vst3` bundle.
- `manguebyte-plugin`: the `Plugin` trait every format wraps; `Harness`,
  which runs a plugin in a unit test with scheduled notes and automation;
  `ParamWatcher`, which turns parameter changes made outside the host into
  events; plugin state as text.
- `plugins/pluck`: an example 16-voice instrument, and
  `scripts/bundle-clap.sh` to build it into a `.clap`.

### Processing model

- `Processor::process_with` takes a `ProcessContext` carrying one
  timestamped `EventBuffer` (notes, per-note expression, parameter values,
  modulation, MIDI), the `Transport` and a sidechain.
- `AudioBuffer::windowed` narrows a buffer to a range of frames without
  copying, and `split_at_events` uses it to apply each event on its exact
  frame.
- `Synth` and `Voice`: polyphonic voice allocation with per-note expression
  and modulation, sustain pedal and voice stealing. The synth remembers
  where the pitch wheel and each controller stand on every channel and
  tells a voice before it starts a note, so a note struck while the wheel
  is held starts bent, whichever voice it lands on.
- `Processor::latency_samples`, summed by chains.
- `ParamId`: a stable 32-bit hash of a parameter's text identifier.

### Graph

- `manguebyte-graph`: a processing graph that is edited while it plays.
  `Graph` is the description and `GraphEngine` the processor; a commit
  compiles a plan and swaps it in through a wait-free queue. Nodes keep
  their state across edits, removed nodes are freed off the audio thread,
  and paths of different latency are aligned automatically.

### Signal processing

- `StateVariableFilter`, stable while its cutoff is modulated at audio rate.
- `Compressor` with soft knee and linked channels; `NoiseGate` with hold.
- `Reverb`: an eight-line feedback delay network.
- `fft::Fft` and `fft::RealFft` for any length; `window::Window` with nine
  shapes.
- `Adsr::silence`.
- `Oversampled`: runs any processor at 2, 4, 8 or 16 times the sample rate
  through linear-phase half-band filters, with its delay reported.
- `Resampler` and `resample`: band-limited conversion between any two
  sample rates, in a stream or for a whole buffer.
- `Limiter`: look-ahead peak limiter whose ceiling holds for every sample.
- `Waveshaper` (hard clip, tanh, soft clip, fold), `DcBlocker`, `Noise`
  (white, pink, brown), `Lfo` with tempo sync, and `fir` design functions.

### Parameters

- `manguebyte-params`: `FloatParam`, `IntParam`, `BoolParam`, `ChoiceParam`;
  linear, skewed, symmetric and logarithmic ranges; text conversion;
  `ParamSmoother`; `ParamSet` with saved state that tolerates version
  changes.

### MIDI

- System common and real-time messages; `StreamParser` for raw byte streams
  with running status and SysEx; reading and writing standard MIDI files.

### File formats

- `flac`: a FLAC decoder written from RFC 9639, covering every subframe
  type, both Rice codings, wasted bits, the three stereo decorrelations and
  4 to 32 bits per sample, with frame checksums verified.
- `aiff`: AIFF and AIFF-C reading (big and little-endian PCM, float) and
  writing (16 and 24-bit PCM, 32-bit float).
- `decode` and `read_file` recognise WAV, AIFF and FLAC from the contents.
- The decoders are tested against files encoded by FFmpeg and must match
  their source sample for sample.

### Devices

- Device listing with stable identifiers; choice of sample rate, channel
  count and buffer size; `InputDevice`; `DuplexStream`.
- `devices::midi`: MIDI input and output ports. `MidiReceiver` stamps each
  message with its arrival time and places it inside the next block at the
  matching frame, so a player's timing is not quantised to the buffer size.
- `manguebyte-standalone`: `Standalone::<P>::start` runs a plugin on the
  default output device, listens to every MIDI input, gives an effect the
  default input, and accepts notes from the program itself.

### Quality

- A counting allocator fails the build if any processor, or the CLAP
  wrapper's `process`, touches the heap while processing.
- The same material rendered with different block sizes must be identical
  bit for bit.
- Miri runs the tests of the two places that contain `unsafe` code.

### Fixed

- A voice that finished in the middle of a block kept advancing until the
  end of it, so the next note depended on the host's block size.
- `ParamSmoother` could stall just short of its target on a long exponential
  ramp and never report that it had finished.
- A value just below zero was displayed as `-0.0`, which read back as a
  different text.

## 0.1.0 (not published)

The foundation: `AudioBuffer`, `Sample`, `Smoothed`, atomic floats, a
wait-free queue, the `Processor` trait and tuple chains, oscillator, biquad
filters, ADSR, delay, gain, MIDI channel messages, WAV files and playback on
the default output device.
