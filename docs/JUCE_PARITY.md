# JUCE parity

Manguebyte's baseline goal is to offer everything JUCE offers. This page
tracks that goal module by module, against the 24 modules in JUCE's `master`
branch (version 9.0.4 when this was written, October 2026).

The module list is taken from the JUCE repository and its documentation
index. The feature summaries are a working description based on JUCE's public
documentation and will be refined into per-feature checklists as each area is
built. JUCE is a reference for scope here, never a source of code: see
[ADR 0004](adr/0004-juce-as-reference.md).

Parity means the same capability, not the same classes. Where Rust or its
ecosystem already solves something well, the plan says so instead of
rebuilding it.

Legend: ✅ done · 🚧 partly there · ⬜ not started · ➖ not planned as part of
Manguebyte

## Audio

| JUCE module | What it covers | Manguebyte | Status |
|---|---|---|---|
| `juce_audio_basics` | Audio buffers, sample conversion, MIDI messages and buffers, MIDI files, MPE, UMP, synthesiser voices, smoothed values, basic sources, ADSR | `core`, `midi`, `dsp` | 🚧 Buffers, smoothing, channel and system messages, SysEx, stream parsing, MIDI files, sample-accurate MIDI buffer and ADSR are done. Missing: MPE, UMP, voice allocation, audio sources |
| `juce_audio_devices` | Device enumeration and selection, input and output streams, per-platform backends (CoreAudio, WASAPI, ASIO, ALSA, JACK, Oboe, and others), MIDI ports, device manager with saved setup | `devices` | 🚧 Listing, selection by stable id, sample rate and buffer size, output, input and duplex are done. Missing: MIDI ports, host selection (ASIO, JACK), hot-plug notification, a device manager with saved setup |
| `juce_audio_formats` | Readers and writers for WAV, AIFF, FLAC, Ogg Vorbis, MP3 and platform codecs; streaming sources; thumbnails cache; sampler | `formats` | 🚧 WAV read and write. Missing: streaming, every other format, metadata |
| `juce_audio_processors` | The plugin and processor model, parameters and their state tree, plugin hosting for VST3, AU, LV2 and others, processor graph, plugin scanning | `params`, later `plugin`, `host`, `graph` | 🚧 Parameters and state are done, with a different design ([ADR 0003](adr/0003-parameters-without-listeners.md)). The processor model, hosting and graph are not started |
| `juce_audio_processors_headless` | The same, without GUI dependencies (added in JUCE 8.0.11) | the same crates | ✅ by construction: no processing crate depends on a GUI |
| `juce_audio_plugin_client` | Wrappers that turn a processor into a VST3, AU, AUv3, LV2, AAX, Unity or standalone plugin | later one crate per format | ⬜ Planned order: CLAP, VST3, standalone, AU, LV2. JUCE has no native CLAP |
| `juce_audio_utils` | Ready-made audio widgets and helpers: device selector, keyboard, thumbnail, transport-style player | later `gui` | ⬜ |
| `juce_dsp` | FFT, windows, IIR and FIR filters with design methods, state-variable and ladder filters, oversampling, convolution, delay lines, dynamics, reverb, chorus, phaser, waveshaping, oscillator, panner, SIMD wrappers, lookup tables | `dsp` | 🚧 Processor chains, oscillator, biquad designs, state-variable filter, delay, gain, compressor, noise gate, reverb, FFT of any length and windows are done. Missing: FIR and ladder filters, high-order IIR design, oversampling, convolution, limiter, chorus, phaser, waveshaper, panner, SIMD, lookup tables |
| `juce_midi_ci` | MIDI Capability Inquiry: discovery, profiles, property exchange | later, in `midi` | ⬜ |
| `juce_osc` | Open Sound Control messages over UDP | later `osc` | ⬜ |

## User interface

| JUCE module | What it covers | Manguebyte | Status |
|---|---|---|---|
| `juce_graphics` | 2D drawing: paths, colours, gradients, images, fonts and text layout, software and platform renderers | later `gui` | ⬜ |
| `juce_gui_basics` | Component tree, windows, mouse and keyboard, layout, look-and-feel, the standard widgets, menus, drag and drop, accessibility | later `gui` | ⬜ |
| `juce_gui_extra` | Code editor, web view, system tray, and other specialised widgets | later `gui` | ⬜ |
| `juce_animation` | Animators and easing | later `gui` | ⬜ |
| `juce_opengl` | OpenGL contexts and rendering into components | later `gui` (GPU rendering is the default path, not an add-on) | ⬜ |
| `juce_video` | Video playback and camera capture | ➖ | Use a dedicated crate |

## Foundation

A large part of JUCE is a general-purpose C++ toolkit that predates the
modern standard library. Rust's standard library and ecosystem cover most of
it, so the plan here is mostly to not rebuild it.

| JUCE module | What it covers | Manguebyte | Status |
|---|---|---|---|
| `juce_core` | Strings, containers, files, streams, threads, time, XML, JSON, networking, memory utilities, logging | Rust `std` and established crates | ➖ except audio-specific pieces: lock-free queue ✅, atomic floats ✅, denormal guard ⬜, high-resolution timer ⬜ |
| `juce_events` | Message loop, timers, async updaters, change broadcasters, inter-process connections | later `gui` for the event loop | ➖ as a general facility. The audio side deliberately has no broadcaster ([ADR 0002](adr/0002-real-time-thread-model.md)) |
| `juce_data_structures` | Value trees, undo manager, properties files | later, if the GUI needs it | ⬜ Undo for parameter changes is planned in the plugin layer |
| `juce_cryptography` | Hashes, RSA, Blowfish | ➖ | Use RustCrypto |
| `juce_javascript` | Embedded JavaScript engine | ➖ | Use a dedicated crate |
| `juce_box2d` | Bundled 2D physics | ➖ | Use a dedicated crate |
| `juce_analytics` | Analytics event collection | ➖ | Out of scope |
| `juce_product_unlocking` | Licence key and online unlock helpers | ➖ for now | Revisit with the plugin tooling |

## Beyond parity

Things JUCE does not offer, or offers only in part, that Manguebyte commits
to. Each one is tied to something observable.

| Area | JUCE today | Manguebyte |
|---|---|---|
| Real-time safety | A convention; the framework's own parameter attachments post to the OS message queue from the audio thread | Enforced: an allocation in `process` fails CI ✅ |
| Parameter threading | Listeners run on whichever thread changes the value, including the audio thread | No listeners; atomics and polling ✅ |
| State and processing | Host may call state functions concurrently with processing; each plugin synchronises | Parameter state is safe from any thread with no lock ✅ |
| Old sessions after a range change | Position-based automation and state can shift meaning | State stores real values by id; loading is tolerant and reports ✅ |
| Headless use | Split out in 8.0.11, still needs the event module | No GUI or event loop below the GUI crate ✅ |
| CLAP | Not available natively; promised for JUCE 9, not in 9.0.x | First plugin format to be implemented ⬜ |
| Sample-accurate automation | Not in `AudioProcessor` | Timestamped events in the process call from the first plugin API ⬜ |
| MIDI 2.0, note expression, per-voice modulation | Not in the processor model | Designed into the event type before any wrapper is written ⬜ |
| Arbitrary FFT length | Powers of two | Any length ✅ |
| Compile-time chains | Template chains exist; graph is dynamic | Tuples of processors inline end to end ✅ |
| Deterministic offline rendering in CI | Possible with effort | A design goal: same processor, no device, bit-exact ⬜ |
| Licence | AGPLv3 or commercial | MIT or Apache-2.0 ✅ |

Sources for the JUCE column: the JUCE repository and changelog, the JUCE
roadmap update of 2024, and the forum threads cited in ADRs 0002 and 0003.
JUCE has announced work on an "AudioProcessor v2" covering sample-accurate
automation and CLAP features, so some rows may change in later 9.x releases.
