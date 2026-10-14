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
- `plugins/duck`: an example effect, a sidechain ducker with a bypass that
  does not click. Both examples are validated as CLAP and as VST 3 in CI.
- `Harness::process_with_sidechain` feeds an effect's sidechain in a test.
- `cargo mangue`, the command-line tool: `new` writes a plugin crate from a
  template (instrument or effect) that builds and tests as it is; `bundle`
  builds a package and writes its `.clap` and `.vst3` with the right layout
  for the platform, finding the library from cargo's own output, with
  universal binaries and an ad-hoc signature on macOS; `install` and
  `uninstall` use the per-user plugin folders; `validate` runs both
  formats' validators and fails on any failed test. Bundle layouts are pure
  functions, tested for every platform on any machine. The two example
  plugins declare their names in `[package.metadata.manguebyte]`.

### Hosting

- `manguebyte-host`: load CLAP and VST 3 plugins made by anyone and run them
  as a `Processor<f32>`, alone or as a node of a graph. The thread rules are
  types: a main-thread handle owns loading, parameters, state and the
  plugin's requests, and activating it yields the audio-thread half, which
  is `Send` and allocates nothing while processing. Every event of an
  `EventBuffer` reaches the plugin on its frame (VST 3 parameter changes as
  queues with one point per event; MIDI controllers through the plugin's own
  mapping), and what the plugin sends back is collected. Strings, counts and
  pointers from a plugin are checked and bounded, and a plugin that fails
  leaves silence and an error, not a crashed host. `any` is one interface
  over both formats. The example plugins, hosted through their real entry
  points, produce output identical bit for bit to `Harness` running them in
  process, in either format. Loading a library is `unsafe`; everything after
  it is safe.

### Metering and analysis

- `manguebyte-analysis`, a new crate. `LoudnessMeter` to ITU-R BS.1770-4,
  EBU R 128 and EBU Tech 3341 and 3342: K-weighting for any sample rate,
  channel weights, momentary, short-term and integrated loudness with both
  gates, loudness range, in bounded memory; every Tech 3341 and 3342 case
  that can be synthesised reads within the document's tolerance.
  `TruePeakMeter` by oversampling. `PeakMeter`, `RmsMeter`, `VuMeter`,
  `PpmMeter` (IEC 60268-10 Type I) and `StereoMeter`.
- `SpectrumAnalyzer`: any size, hop and window, ballistics, peak hold, A and
  C weighting, fractional-octave bands to IEC 61260-1, published whole to a
  user interface through a wait-free triple buffer.
- `PitchDetector`: YIN and the McLeod pitch method with a band-limited
  refinement; 1.8 cents worst case at 44.1 and 48 kHz on the tested tones,
  with no octave errors on them; `Note` names the nearest note.
- All of it runs on the audio thread without allocating and publishes its
  readings through atomics any thread can read.

### Open Sound Control

- `manguebyte-osc`, a new crate: messages, bundles and every argument type;
  encoding into the caller's buffer and strict decoding, including a
  borrowed decoder that does not allocate; address patterns with the 1.1
  `//` wildcard, matched in time bounded by the pattern and address
  lengths; UDP (unicast, broadcast, multicast) and TCP with the 1.1 SLIP
  framing. A real-time bridge decodes on a network thread and hands fixed
  events to the audio thread, where a time-tagged bundle lands on its exact
  frame.

### Real-time safety

- `NoDenormals`: a guard that makes the processor treat subnormal numbers
  as zero on the current thread, so a fading sound cannot slow it down. The
  CLAP and VST 3 wrappers, the standalone engine, the device streams and
  `Harness` hold it around every block, so plugins need not. See ADR 0006.
- Every crate that needs no `unsafe` code forbids it
  (`#![forbid(unsafe_code)]`): the facade, devices, signal processing, file
  formats, graph, MIDI, parameters, standalone, OSC, plugin, graphics and the
  command-line tool. The `unsafe` that remains is listed in
  `docs/ARCHITECTURE.md`.
- `LogQueue` (`manguebyte_core::log`): a log the audio thread can write to.
  Posting is wait-free and never allocates, even with formatting
  (`post_fmt(level, format_args!(..))` formats into a buffer on the stack);
  messages below the queue's level are dropped before being formatted; any
  number of threads may post; the draining thread gets the messages in the
  order they were posted, across drains, and a count of the ones that found
  no room. A counting-allocator test and Miri check it.

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

- `Chorus` (1 to 8 voices, stereo spread, feedback, so also a flanger),
  `Phaser` (2 to 12 stages), `DryWet` (dry path delayed to match the
  effect's latency, seven mix laws), `Panner` (the standard pan laws) and
  `EnvelopeFollower` (peak or RMS ballistics).
- `DelayLine` reads between samples by linear, cubic or all-pass (Thiran)
  interpolation.
- `OnePole` and `LadderFilter` (a four-pole ladder solved per sample,
  12 and 24 dB low, high and band-pass, self-oscillating at full
  resonance), both stable under audio-rate modulation.
- `iir`: Butterworth, Chebyshev I and II and elliptic designs of order 1 to
  16 in four band types; `BiquadCascade` and `IirFilter` glide between any
  two designs without the state gaining energy.
- `Crossover`: Linkwitz-Riley splitters of 2 to 5 bands whose sum is flat.
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
- `ump`: Universal MIDI Packets with typed access to every message of
  UMP 1.1.1, flex data and stream messages included, checked against the
  specification's own examples.
- `translate`: MIDI 1.0 to 2.0 and back as the specification prescribes,
  scaling verified exhaustively for every 7-bit and 14-bit input.
- `rpn` (RPN and NRPN), `mpe` (zones, configuration message, channel
  allocator, note tracker) and `keyboard` (keys down with the pedal, read by
  an interface while the audio thread writes, never showing a held key as
  released).

### File formats

- `AudioReader` reads WAV, AIFF and FLAC block by block from any reader,
  and seeks to any frame exactly (FLAC through its seek table, which is
  never trusted blindly, or by bisection); memory does not grow with the
  file. `AudioWriter` writes WAV, AIFF and FLAC block by block.
- `StreamingPlayer` and `RecordingWriter` play from and record to disk from
  an audio callback: a worker thread does the I/O and the audio thread only
  touches wait-free queues, counting underruns and overruns instead of
  waiting.
- A FLAC encoder written from RFC 9639: fixed and LPC predictors, all four
  stereo decorrelations, Rice coding with partition search, MD5 and seek
  table, levels 0 to 8, 4 to 32 bits. Everything it writes decodes exactly
  with this crate and with FFmpeg.
- WAV files larger than 4 GB are written as RF64, and `wav::read`, `decode`
  and `read_file` now read RF64 instead of refusing it.
- `flac::read` and `decode` now return an error for a frame whose
  checksums pass but whose samples do not fit the stream's bit depth;
  before, it panicked in debug builds and decoded to noise in release.
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
- Miri runs the tests of the places that contain `unsafe` code that
  handles memory: the queue, the triple buffer, both wrappers and the host.
- CI runs `cargo mangue`'s end-to-end test on Windows, Linux and macOS:
  generate a plugin of each kind, build it, test it and bundle it.

### Fixed

- `Gain` was not identical bit for bit across block sizes while it glided:
  `Smoothed::skip` added a multiple of the step where stepping adds it once
  per sample, and setting the same target again restarted the glide. Both
  are fixed, with a test that glides through blocks of many sizes.
- `AudioBuffer::peak` passed over a NaN, so a test such as `peak() < 1.0`
  could pass on garbage. A NaN now makes the peak NaN.
- `Transport::advance` could overflow in a debug build; it now saturates.
- The CLAP wrapper passed note-expression pan through unconverted: CLAP's
  runs from 0 to 1 and the framework's from -1 to 1.
- `scripts/bundle-vst3.sh` named the 32-bit Linux folder `x86-linux`; VST 3
  calls it `i386-linux`.
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
