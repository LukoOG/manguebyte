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
- `manguebyte-lv2`: LV2 support. `export_lv2!(MyPlugin)` defines
  `lv2_descriptor` for the same `Plugin`, and `bundle_files::<MyPlugin>`
  gives the text of the bundle's `manifest.ttl` and of the plugin's own
  Turtle file. The
  plugin's URI is `urn:manguebyte:` followed by its id. Audio ports, with
  the sidechain marked `lv2:isSideChain`; one control port per parameter,
  in the parameter's own unit and range, whose changes become parameter
  events on frame 0; an atom input carrying MIDI, each message on its
  frame, and `time:Position`; an atom `notify` output, written empty; a
  latency port; state through `state:interface`. `run` never allocates
  (checked with the counting allocator) and flushes subnormal numbers; a
  panic inside the plugin leaves a silent block, and the next block gives
  the plugin every parameter's value again. Tested in process through the
  real C descriptor, acting as a host. `cargo mangue bundle --format lv2`
  writes the bundle (below), and CI loads the example plugins' bundles
  with lilv's `lv2ls` and `lv2info`; `lv2lint`, which the Ubuntu archive
  does not carry, has not read them, and no LV2 host has run a plugin yet.
  `export_lv2!` also defines a hidden function at the crate root,
  `__manguebyte_lv2_bundle_files`, through which `cargo mangue` gets the
  Turtle files.
  - Transport: a block is cut at each `time:Position` inside it, and the
    transport moves on by itself between positions. LV2 counts beats in
    `time:beatUnit` notes and the framework in quarter notes, so tempo and
    positions are converted. The start of the bar is `time:beat` minus
    `time:barBeat` when the host gives both, and `time:bar` times
    `time:beatsPerBar` only when it does not give the running beat. A
    position stamped past the end of a block is moved back by the
    difference, at its own speed and tempo, and holds from the start of the
    next block. A value that does not come out finite is passed on as
    unknown.
  - Turtle: a parameter with whole-number steps over a whole-number range
    is an `lv2:integer` port (`lv2:toggled` for a switch), and its values
    are listed as scale points with `lv2:enumeration` only when they have
    names, as a choice's options do; any other stepped parameter gets
    `pprops:rangeSteps`, the number of values it can take.
  - Versions: LV2 gives `lv2:minorVersion` 0 to experimental plugins and
    odd numbers to development versions, so a semantic version is not
    copied across. `lv2:minorVersion` is 2 × (1000 × major + minor) + 2 and
    `lv2:microVersion` is 2 × patch + 2, or 2 × patch + 1 for a pre-release:
    a release is never marked experimental or in development, and versions
    keep their order. `0.1.0` becomes 4 and 2. The URI does not change
    with the major version.
- `manguebyte-plugin`: the `Plugin` trait every format wraps; `Harness`,
  which runs a plugin in a unit test with scheduled notes and automation;
  `ParamWatcher`, which turns parameter changes made outside the host into
  events; plugin state as text.
- `plugins/pluck`: an example 16-voice instrument, and
  `scripts/bundle-clap.sh` to build it into a `.clap`.
- `plugins/duck`: an example effect, a sidechain ducker with a bypass that
  does not click. Both examples are validated as CLAP and as VST 3 in CI,
  and both are exported as LV2 too.
- Plugins declare the MIDI controllers they listen to:
  `Plugin::MIDI_CONTROLLERS`, a `MidiControllers` set built in a const
  (`NONE`, `MUSICAL`, the default, `ALL`, `list`, `with`, `without`,
  `per_channel`, `contains`, `wants` and more) of `MidiController`s (a
  control change, made with `MidiController::control(n)`, which refuses a
  number above 127, or `CHANNEL_PRESSURE` and `PITCH_BEND`). `MUSICAL` is
  pitch bend, channel pressure and CC 1, 2, 7, 10, 11, 64, 66, 67, 120, 121
  and 123. The VST 3 wrapper exposes only those: one hidden parameter for
  each, shared by every channel, so a VST 3 instrument shows 13 hidden
  parameters by default instead of 2,080 (Mangue Pluck shows 6 + 13). A
  move of a shared controller reaches the plugin on all 16 channels, one
  message per channel in order, so that `Synth` applies it to notes on any
  channel; `per_channel()` gives each channel parameters of its own and
  keeps channels apart. The ids of channels 2 to 16 are gone unless a
  plugin asks for them, and a NaN point on a controller parameter is
  dropped. CLAP is not affected.
- VST 3: the view shows the editor with `OpenEditor::set_visible(true)`
  once `attached` has succeeded, since VST 3 has no call that shows a view,
  and hides it with `set_visible(false)` at the start of `removed`.
- Mangue Duck has an editor: a title, the Bypass switch, knobs for
  Threshold, Depth, Attack and Release showing their values in the
  parameters' own words, and a Reduction meter from 0 to 40 dB, in a fixed
  layout of 440 by 210 logical pixels. The processor feeds the meter
  through a `ReductionMeter` (`Duck::reduction_meter`), written without
  allocating once per block; `editor()` takes it, and the meter falls to 0
  dB once nothing has been published for `METER_HOLD` (500 ms). A sample
  of the sidechain that is not finite counts as silence, where one
  infinite sample used to stop the ducking until a reset.
- Mangue Pluck has an editor with a knob for each of its six parameters
  (520 by 160). It is now in tune while its filter closes, within 0.15 cent
  at the settings its tests measure, by following the filter's phase with
  the new `StateVariableFilter::phase_response`; it never had the
  Karplus-Strong delay its documentation once spoke of. Its output stays
  under -1 dBFS with every parameter at its maximum: a resonance
  compensation, a trimmed reverb, a share of the headroom per voice, a
  soft knee from -3 dBFS (`OUTPUT_KNEE_DB`) and a limiter at -1 dBFS
  (`OUTPUT_CEILING_DB`) with no latency, so nothing comes out flat at the
  ceiling. The reverb's blend and the level follow their events on the
  exact frame, so renders are identical bit for bit whatever the blocks and
  after any reset. The default level is now -4 dB (`DEFAULT_LEVEL_DB`), up
  from -12 dB: with the new headroom a note at -12 dB had become about 12
  dB quieter than before, and at -4 dB a chord of three or four notes
  around middle C peaks at about -6 dBFS. NaN and extreme values in note
  expressions, cutoff modulation and the pitch wheel are guarded against.
- Both example plugins read every parameter event through
  `Param::stored_plain`: a NaN is ignored and a value out of range is held
  to the range.
- A test loads both example plugins' libraries into one process on
  Windows, opens both editors in real windows through the CLAP `gui`
  extension, reads their pixels back, fuzzes the `gui` calls and unloads
  the libraries in either order, leaving no window, GDI object or window
  class behind (ADR 0007, item 8).
- The facade crate has the features `lv2`, `gui` (re-exporting
  `manguebyte-gui` as `gui` and `manguebyte-ui` as `ui`) and `sampler`,
  and its prelude has `Convolution` and its engine, loader, routing and
  settings, `FirFilter`, `FirMode`, `Expander`, `KeySource`, `Wavetable`,
  `WavetableOscillator` and `split_at_events_with`.
- `Harness::process_with_sidechain` feeds an effect's sidechain in a test.
- CLAP: the `gui`, `timer-support` and `posix-fd-support` extensions for
  plugins with an editor, embedded windows only. Gestures reach the host as
  gesture events from `process` and from `params.flush`, which now writes
  its output list; when the host is not processing, an edit asks it for a
  flush. Events the host's list refuses are sent again. Every `gui` call is
  safe in any order, including from inside the editor's own calls.
- VST 3: the `IComponentHandler` the host gives is kept and used for edits
  (`beginEdit`, `performEdit` with the value the control set, `endEdit`, on
  the main thread). `createView` returns an `IPlugView` with content scale
  for plugins with an editor, keeps the `IPlugFrame` for resizing and uses
  the host's `IRunLoop` for timers. A view outliving its plugin, attached
  twice or removed without being attached does nothing harmful.
- Fixed: an edit that the interface stored while a block was being prepared
  could be undone by that block, in the CLAP and VST 3 wrappers and in the
  standalone engine.
- `cargo mangue`, the command-line tool: `new` writes a plugin crate from a
  template (instrument or effect) that builds and tests as it is; `bundle`
  builds a package and writes its `.clap` and `.vst3` with the right layout
  for the platform, finding the library from cargo's own output, with
  universal binaries and an ad-hoc signature on macOS; `install` and
  `uninstall` use the per-user plugin folders; `validate` runs both
  formats' validators and fails on any failed test. Bundle layouts are pure
  functions, tested for every platform on any machine. The two example
  plugins declare their names in `[package.metadata.manguebyte]`.
- `cargo mangue bundle --format lv2` writes `<Name>.lv2`: the library
  (`Name.dll`, `Name.so` or `Name.dylib`, one universal binary on macOS,
  where the library is signed ad hoc) and the Turtle files. Only the
  plugin's code can describe it, so the tool writes, builds and runs a
  small program against the plugin's crate, in the plugin's target folder
  with its profile, its features and a copy of its workspace's
  `Cargo.lock`; the crate must invoke `export_lv2!` at its root and have
  the `rlib` type. LV2 is bundled only when asked for (`Format::DEFAULT`
  is CLAP and VST 3); `--all-features` is refused for it. `install` and
  `uninstall` know LV2's folders, and `validate`, which has no LV2
  validator, refuses LV2. New crates from `cargo mangue new` enable the
  facade's `lv2` feature and export LV2 too.

### User interface

Phase 1 of [ADR 0007](docs/adr/0007-user-interface-layer.md), up to its
item 8. Windows exist on Win32 only; everything else is tested without a
display, and no editor has been opened inside a real host yet.

- `manguebyte_plugin::editor`: the editor contract. `Plugin::editor` returns
  an optional `EditorSpec`, which is `Send + Sync + 'static`, so it cannot
  hold the processor, a borrow, an `Rc` or a raw pointer (compile-fail tests
  prove each). An open editor is `!Send` and closes when dropped. Editors
  reach the host through an `EditorContext` that the wrapper cuts off when
  the editor closes: open gestures are ended, timers and descriptor watches
  cancelled, and every clone does nothing from then on. `Gesture` is the
  only way an edit reaches the host: begin, set, end, and cancel to restore
  the starting value; gestures on one parameter from several places collapse
  into one. `Resize` rules with a pure `fit`, `ParentWindow`, key events,
  main-thread wakes, and test doubles (`RecordingEditorHost`, `ManualLoop`).
  `Harness::editor` exposes a plugin's editor in tests.
- `manguebyte-gfx`: scenes recorded as display lists (rectangles, rounded
  rectangles, circles, arcs and paths, filled or stroked, with solid colour,
  linear and radial gradients, images, clips, transforms and opacity layers,
  and a columns primitive for meters and spectra), an exact text dump of a
  scene, PNG reading and writing, and a CPU rasteriser behind a private seam
  on `tiny-skia`. A repaint limited to damage rectangles is identical to a
  full repaint, bit for bit, and leaves every other pixel untouched. Text:
  `TextStyle` (size, weight, optical size, tabular figures), `TextLayout`
  (line breaking, alignment, ellipsis, caret positions) and a bounded glyph
  cache, using an embedded subset of Inter (`TYPEFACE_LICENCE` carries its
  OFL text). Golden images and dumps, fuzzing and a benchmark.
- `manguebyte-ui`: a retained widget tree whose views are plain functions;
  values are sources polled once per frame, with no listeners; a layout of
  its own (row, column, stack, padding, spacing, weights, limits); theme
  tokens; label, knob, slider, toggle, meter and canvas widgets; parameter
  edits made only through `Gesture`, showing the value being sent while a
  gesture is open; an accessibility model whose method every widget must
  implement; damage tracking, so a steady interface repaints nothing and
  touches no memory; and a harness that finds widgets by role and name and
  drives them with pointer, wheel and keys on a clock the test controls.
- `manguebyte-window`: platform windows. `Window::open` makes a child of the
  host's window, on Win32 so far; `Window::headless` makes a window with no
  system window behind it, on every system, which a test drives through a
  `Headless` controller. A `WindowHandler` receives `Event`s on the
  window's thread: paint with its damage, pointer down, move, up, leave and
  cancel, wheel turns in lines, keys with their modifiers and a repeat
  flag, text, focus, resizes, scale changes, timers and closing. A window
  presents a premultiplied pixmap through a back buffer of its own,
  converting and copying only the damaged rectangles, and answers an
  expose from that buffer without asking the handler. The two backends
  share the damage, back buffer, pointer, timer and dispatch code.
  - Win32: a `WS_CHILD` window presenting with `SetDIBitsToDevice`; the
    pointer captured from the first button down to the last button up;
    timers owned by the window. A message that arrives while the handler
    runs waits and is delivered after it returns, so the handler is never
    re-entered; while it waits, repeated timer ticks, pointer moves and
    paints are merged, at most `MAX_QUEUED` (1024) events wait, and
    `Window::dropped_events` counts any beyond that. Keys, characters and
    wheel turns the handler declines go on to the parent window; Tab, Enter
    and Escape are left to a host's dialog unless
    `Window::set_wants_all_keys(true)` asks for them. A parent window owned
    by another process is refused. The class, hook, thread and DPI rules
    of ADR 0007 are kept and recorded there.
  - The `testing` feature adds `win32::testing`: test parent windows,
    message injection, message loops and pixel read-back. `ReadBack::Screen`
    reads the pixels on the window's surface; `ReadBack::PrintClient` only
    checks the `WM_PRINTCLIENT` answer. A pixel test fails when the session
    cannot read windows back, unless `MANGUEBYTE_WINDOW_SKIP_READ_BACK` is
    set, which CI never does.
  - `examples/probe.rs` presents a known pattern and reads it back: it
    exits 0 on `PASS` (or `SKIP` on a system with no window backend yet),
    1 on `FAIL` and 2 on `CANNOT`, when the session cannot read windows
    back. CI runs it on Windows.
- `manguebyte-gui`: the glue between the plugin wrappers and the widgets.
  `Editor` is an `EditorSpec` made from a size in logical pixels, resize
  rules, an optional theme and a function that builds the view; a plugin
  returns it from `Plugin::editor`. Opened, it makes a `manguebyte-window`
  child of the host's window (Win32 only; other parents are refused with
  `EditorError::Unsupported`), builds a `Root` on the wrapper's
  `EditorContext`, sends it the window's input in logical pixels, and runs
  a frame about every 16 ms (`FRAME_PERIOD_MS`) from a timer the window
  owns while the editor is visible. A frame where nothing changed polls the
  parameters and sources and then lays out, draws and presents nothing;
  the timer is stopped while the editor is hidden. A hidden editor only
  takes note of new sizes and scales, and draws them once when shown; a
  new scale and the size the host grants for it are drawn in one frame. No
  size the editor opens at, accepts or asks for is larger than
  `manguebyte_ui::MAX_BUFFER_SIDE` device pixels a side. An interface that
  panics while it handles an event or draws is dropped at once, which ends
  its gestures and calls `EditorContext::closed`. An editor opens hidden,
  with no frame timer, and starts when the host shows it through
  `OpenEditor::set_visible`: CLAP's `show`, or the VST 3 view once
  `attached` has succeeded.
  - The `headless` feature adds `Editor::on_headless_open` and
    `HeadlessEditor`, and only adds API: an editor opens headless only when
    it was given a hook, so `cargo test --workspace` leaves every other
    editor alone. Tests drive an editor with a headless hook end to end
    through the real CLAP interface and VST 3's COM interfaces, open and
    close it thousands of times while `process` runs under the counting
    allocator, fuzz the hosts' calls, and compare it with golden images at
    scales 1 and 2. On Windows, a test embeds it in a real window and reads
    its pixels back.
- `Param::stored_plain` says what a parameter would store for a value,
  without storing it.

### Sampler

- `manguebyte-sampler`, a new crate: an SFZ player. `Sfz::parse` reads the
  `<control>`, `<global>`, `<master>`, `<group>` and `<region>` headers
  with inheritance, comments, `#define` and `#include` (through a sample
  source, with cycle, depth and count limits), `default_path`,
  `note_offset` and `octave_offset`; opcodes it does not play are kept and
  reported as warnings, out-of-range values are clamped with a warning, and
  unreadable text is an `SfzError` with its file and line. Inherited
  opcodes are shared, and the opcodes a region takes are capped
  (`sfz::MAX_REGION_OPCODES`).
- `Instrument::from_sfz` loads the samples through a `SampleSource`:
  `FileSource` for a folder, `MemorySource` for bytes handed over by a
  browser. They are decoded by `manguebyte-formats`; a WAV `smpl` chunk
  gives loop points and the root key, and `*sine` and `*silence` are
  generated.
- `Sampler` is a processor that takes its instrument whole from a
  `SamplerLoader` through the framework's swap pattern: notes already
  playing keep the old instrument, which goes back to the loader to be
  freed once its last note ends. Positions are fixed-point 32.32, so pitch
  is exact. Interpolation is a 32-tap windowed sinc whose kernel stretches
  to keep pitched-up samples from aliasing, or `Interpolation::Cubic`, the
  cheaper choice. Sample-accurate events, velocity layers, round robin,
  seeded random layers, release, first and legato triggers, `rt_decay`, all
  four loop modes with a precomputed crossfade, a DAHDSR envelope, choke
  groups, group polyphony and voice stealing that fades instead of
  cutting; the pitch wheel, the sustain pedal, all sound off, reset all
  controllers, all notes off, note chokes and per-note volume, pan and
  tuning. `SamplerVoice` plays an
  instrument as a `Voice` for `Synth`, with documented limits.
- It builds for `wasm32-unknown-unknown`. `examples/wasm_smoke.rs`, run in
  Node.js, plays notes with no host imports and measures their pitch
  within 0.0003 cents; `examples/sampler_bench.rs` and
  `examples/wasm_bench.rs` time it natively and in WebAssembly.
- `InstrumentBuilder` loads an instrument one sample at a time (`paths`,
  `load`, `finish`), for threads that may only work in short steps, and
  gives what `Instrument::load` gives. `Instrument::covers_key` says
  whether a key starts a region. `Sampler::release_all(shortest)` lets go
  of every note as all-notes-off would, but never faster than `shortest`
  (one-shots and short releases fade in a straight line instead), and
  `Sampler::fade_out_all(seconds)` fades every voice; both leave every key
  and pedal up. The windowed-sinc table is now made once per program and
  shared by every instrument, instead of once per instrument (a few
  milliseconds each); `prepare_tables` makes it ahead of time.

### Web

- `manguebyte-web`, a new crate: an engine for the browser that builds,
  runs and is tested natively too. Numbered parts play an eight-voice synth
  or a General MIDI drum kit through an optional low-pass filter, an
  optional reverb, gain and pan. Events are 32-byte little-endian records
  (frame, kind, part, note, channel, velocity, parameter, value) that land
  on their exact frame; the output is identical bit for bit whatever the
  size of the quanta. Records arrive through a single-producer
  single-consumer ring with a fixed byte layout, which in a browser lives
  in a `SharedArrayBuffer`, or are copied in messages without shared
  memory. Parameters have stable text ids and are described as JSON in the
  `manguebyte-params` format; the drum map gives each drum its General
  MIDI note and the symbol Aratu writes for it. The record layout, the
  ring and the drum map are the contract with Aratu, documented in the
  crate and its README.
- Built for `wasm32-unknown-unknown`, it is a module with plain
  `extern "C"` exports (ABI version 2) and no imports. `js/worklet.js` is
  the `AudioWorkletProcessor`, `js/engine.js` the main-thread helper that
  maps `AudioContext` time to frames, and `js/ring.js` the ring; 39 tests
  run them in Node.js against the built module. `examples/web` is a demo
  page with a server that sends the cross-origin isolation headers.
- The output goes through a limiter with no look-ahead that keeps every
  sample within `OUTPUT_CEILING` (about -0.3 dB). Sample rates from 3 to
  768 kHz are accepted.
- Accents: byte 9 of a record, which was padding, is now `flags`, and its
  bit 0 accents a note on, as Aratu marks a note written in capitals; the
  other bits are reserved. An accented synth note is 3.5 dB louder, opens
  its filter an octave further and attacks twice as fast; an accented drum
  is 3 dB louder with its attack sharpened. A 0 there means no accent, so
  records from older writers play as before and the ABI version stays 2.
  `EventRecord::with_accent`, `encodeRecord({ accent })` and the sixth
  argument of `noteOn` in `js/engine.js` write it.
- Five more drums, after the first six in the drum map: high tom (50,
  `t`), mid tom (47, `m`), floor tom (43, `f`), side stick (37, `k`) and a
  guitar body tap (88, `guitar_tap`, no symbol: `null` in the JSON). The
  toms are tuned membranes that fall in pitch as they settle, with the
  stick's noise; the side stick a short woody knock; the tap the thump of
  a guitar's air and top resonances with a nail's click. Each has a level
  parameter (`high_tom.level` and so on). The `drum_wavs` example writes
  every drum to a WAV file to listen to.
- Sampler parts (`InstrumentKind::Sampler`, code 2, `'sampler'` in
  `js/engine.js`) play SFZ instruments through `manguebyte-sampler`, with
  velocity, accents (3 dB louder, `SAMPLER_ACCENT_DB`), note off, choke
  and an all-notes-off that never takes less than 8 ms
  (`PANIC_FADE_SECONDS`); samples play at their own rate at the right
  ratio, read with cubic interpolation, or a windowed sinc through the
  `sampler.interpolation` parameter. `SfzLoad` loads an instrument a file
  at a time and `Engine::load_instrument` gives it to a part, whose
  sounding notes ring on with the old one. `await engine.loadSfz(part,
  url)` fetches the SFZ file and its samples; the worklet decodes them
  between render quanta, a step of at most about one file at a time, so
  loading never holds the audio up and `mb_render` never allocates. New
  exports `mb_text_ptr`, `mb_input_ptr`, `mb_sfz_begin`,
  `mb_sfz_files_json`, `mb_sfz_add_file`, `mb_sfz_finish`,
  `mb_sfz_abandon` and `mb_collect` are added beside the old ones, which
  mean what they did, so the ABI version stays 2; `js/worklet.js` looks
  for them by name. Every instrument Aratu ships loads with no warning
  (`tests/aratu.rs`, when Aratu's samples are on the machine).
- Sampler kits (`InstrumentKind::SamplerKit`, code 3, `'sampler_kit'`):
  sampler parts marked as drum kits, which play the synthesized drum for a
  note of the drum map their SFZ text has no region for (Aratu's kit has
  no guitar tap, 88), at its velocity, with its accent and through the
  part's chain, and every drum until their samples arrive. They have the
  kit's parameters for those drums. The fallback is chosen with the kind
  of part, not guessed, so that a melodic instrument with a gap in its
  keys never plays a drum.
- Playing live. Record kind 6, cancel (`EventRecord::cancel`,
  `engine.cancel(part, time)`): every event of its part due on its frame
  or later that reached the engine before it is dropped (counted in
  `EngineStats::cancelled`, `mb_stat` 10), and on that frame the part's
  sounding notes fade out over 8 ms (`CANCEL_FADE_SECONDS`), on every kind
  of part, so an edited bar can be replaced mid-way; the code was one every
  engine ignored, so the ABI version stays 2. All notes off is shown never
  to click on any kind of part: the largest step after it is never larger
  than the largest before (a cut adds 73 to 92% of the peak).
  `Engine::latency_frames`, `mb_latency_frames` and `engine.latency()` give
  the engine's latency, one render quantum, and with the context's output
  latency the whole of it.

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
  formats, graph, MIDI, parameters, standalone, OSC, plugin, graphics,
  widgets, the interface glue (`manguebyte-gui`) and the command-line tool;
  `manguebyte-window` denies it everywhere but its Win32 backend. The
  `unsafe` that remains is listed in `docs/ARCHITECTURE.md`.
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
- `ProcessContext` has a new public field, `offset`: where the buffer
  begins within the host's block, so that the pieces of a split block read
  the sidechain at their own frames. `split_at_events_with` hands each
  piece its own context, as does `ProcessContext::for_piece`, and
  `sidechain_peak` gives the sidechain's level at a frame of the piece. The
  struct is `#[non_exhaustive]`, so the field breaks no other crate. A processor
  that reads the sidechain must be given the piece's context; the older
  pattern of `split_at_events` with `without_events()` leaves the
  sidechain misaligned after the first piece.
- `Synth` and `Voice`: polyphonic voice allocation with per-note expression
  and modulation, sustain pedal and voice stealing. The synth remembers
  where the pitch wheel and each controller stand on every channel and
  tells a voice before it starts a note, so a note struck while the wheel
  is held starts bent, whichever voice it lands on.
- `Synth`: a free voice is chosen by its position, not as the oldest, and
  `reset` also clears the clock of voice ages (it keeps the pitch wheel and
  the controllers, as before), so playing the same material again, with or
  without a reset, gives the same output bit for bit. `note_on` and
  `note_off` hold the velocity to 0 to 1 and take a velocity that is not
  finite as 0.
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
- `Expander`: downward expansion with threshold, ratio (an infinite ratio
  makes it a gate), soft knee, attack, release, a range of at most 240 dB
  and a gain-reduction meter.
- Sidechain keying for `Compressor`, `NoiseGate` and `Expander`, opt-in:
  `with_key_source(KeySource::Sidechain)` makes one listen to the context's
  sidechain, and `KeySource::Input`, the default, keeps it on its own
  input. A wrapper that always passes a sidechain, silent when the host
  leaves it unconnected, changes nothing unless a processor asks for it,
  and each processor in a chain chooses on its own. `Oversampled` does not
  pass the sidechain to the processor inside it.
- `Convolution` with an impulse response of any length, by uniformly
  partitioned overlap-save in the frequency domain. In zero-latency mode,
  the default, the first partition is convolved directly in the time
  domain and the output is not delayed; otherwise it is late by exactly one
  partition. The output is identical bit for bit whatever the host's block
  sizes, while the response stays the same. Routings: one response for
  every channel or one per channel, and true stereo (left to left, left to
  right, right to left, right to right). The sums of the older partitions
  are spread over the samples of each partition, so with host blocks
  smaller than a partition no block carries all of them. Against direct
  convolution in double precision, the largest error measured is 1.47e-6
  of the output's RMS level in single precision, for responses of 1 to
  100,000 samples (the test fails above 2e-6), and below 1e-13 in double
  precision up to 20,000 samples. A `ConvolutionEngine` is built off the
  audio thread and handed to the playing `Convolution` by its
  `ConvolutionLoader` through a wait-free queue, with an optional
  crossfade; the old engine travels back to be dropped there, so the audio
  thread never allocates or frees. An engine that arrives during a
  crossfade waits for it to end. `ConvolutionLoader::latency` gives the
  latency of the engine sent last, so a plugin can tell when a new
  response changes it.
- `FirFilter`: a FIR filter processor for any coefficients, such as those
  of the `fir` design functions, with no latency. It chooses on its own
  between direct convolution and the partitioned FFT engine, by the number
  of multiplications per sample, or takes `FirMode` to force one.
- `WavetableOscillator` and `Wavetable`: single-cycle frames of any
  length, split into at most 2,048 harmonics and built off the audio thread
  into a mip-map of two levels per octave, each read with fifth-order
  Lagrange interpolation; the oscillator plays the level whose harmonics
  stay below half the sample rate. It morphs between frames, with an
  optional glide, takes phase modulation from its input, and is identical
  bit for bit across block sizes. A table is shared through an `Arc`, and
  `set_table` hands the old one back so that the audio thread never drops
  it. A sawtooth played at 48 kHz from A0 to C8 keeps the energy away from
  its harmonics below -125 dB, the test's limit (the worst measured was
  -131.4 dB, with the measurement's own floor below -150 dB), and stays
  within -120 dB of the ideal band-limited sawtooth.
- `Reverb`: an eight-line feedback delay network.
- `fft::Fft` and `fft::RealFft` for any length; `window::Window` with nine
  shapes.
- `Adsr::silence`.
- `Oversampled`: runs any processor at 2, 4, 8 or 16 times the sample rate
  through linear-phase half-band filters, with its delay reported.
- `Resampler` and `resample`: band-limited conversion between any two
  sample rates, in a stream or for a whole buffer.
- `Limiter`: look-ahead peak limiter whose ceiling holds for every sample.
  A sample that is not finite comes out as silence and leaves the gain as
  it was.
- `StateVariableFilter::phase_response`, the filter's exact phase at a
  frequency in every mode, and `StateVariableFilter::coefficients`.
- `Waveshaper` (hard clip, tanh, soft clip, fold), `DcBlocker`, `Noise`
  (white, pink, brown), `Lfo` with tempo sync, and `fir` design functions.
- `curve`: the curves an interface draws, computed from the settings the
  processor runs on rather than imitated. Each is a small `Copy` value,
  taken from the processor (`Filter::response`, `Compressor::transfer`,
  `Adsr::shape`, `Chorus::sweep`, `Delay::taps`, `Reverb::decay` and so
  on) or made from parameter values on any thread with the same limits;
  nothing allocates. Frequency responses (`FrequencyResponse`: complex,
  magnitude in dB, phase) of `Biquad` and `Filter` (with their
  coefficients rounded as they are held), `BiquadCascade`, `IirFilter` and
  `iir::Design` (from the form the cascade runs, exact where the
  coefficients are not), `StateVariableFilter`, `OnePole`, the linear model
  of `LadderFilter`, every band of a `Crossover` and their sum, and
  `FirFilter`; a slice, array or tuple of them is a chain whose decibels
  add up. Static transfer curves of `Compressor` (knee and makeup),
  `Expander`, `NoiseGate`, `Limiter` and `Waveshaper`; the corners of an
  `Adsr`; one cycle of an `Lfo`; the delay of each `Chorus` voice and the
  frequency of the `Phaser` stages over a cycle; the time and gain of each
  echo of a `Delay`; and the RT60 of a `Reverb` against frequency, from an
  energy model of its network. `CurveSpec` says what a curve is and how to
  draw its axes (unit, linear or logarithmic, a fitted range), and
  `log_spaced`, `Axis::fill` and `unwrap_phase` make the grids. Every
  curve was checked against the processor itself: frequency responses
  against impulse responses up to 0.45 of the sample rate (the largest
  difference measured was 4e-9 dB and 2e-11 rad, the tests allow 1e-7)
  and steady sines (1e-12 dB), transfer curves against settled steady
  levels (within 1e-9 dB), envelopes, sweeps and echoes sample by sample,
  and the reverb's RT60 against impulse responses rendered offline (see
  `ReverbDecay` for how close). The `response_curves` example writes them
  all as SVG and CSV.

### Parameters

- `manguebyte-params`: `FloatParam`, `IntParam`, `BoolParam`, `ChoiceParam`;
  linear, skewed, symmetric and logarithmic ranges; text conversion;
  `ParamSmoother`; `ParamSet` with saved state that tolerates version
  changes.
- Parameter metadata as plain data: `Param::describe` gives a
  `ParamDescription` (id, `param_id`, name, unit, min, max, default, a
  `ParamScale`, an optional smoothing and the flags), and
  `ParamSet::descriptions` and `descriptions_json` give every parameter's,
  in declaration order. A `ParamScale` is linear, logarithmic, skewed or
  stepped (with the step, the names of a choice's options and, for a
  stepped float on a curve or one whose last step falls short of the end
  of the range, its `Curve` and `end`); each documents its formula from
  value to position, so an editor in another language draws the same
  curve. The JSON is written by hand, one object per parameter with the
  keys `id`, `param_id`, `name`, `unit`, `min`, `max`, `default`, `scale`,
  `smoothing_ms` (only when the parameter declares a smoothing) and
  `flags`, escaped as RFC 8259 asks and with U+2028 and U+2029 escaped too;
  a float parameter's numbers are the shortest decimals that read back as
  the same `f32`.
- `Param::display_plain` and `Param::parse_display` turn any value into
  text with its unit and back; `ParamSet::value_to_text` and
  `text_to_value` do it by id. A choice's or a switch's names are matched
  exactly first, then trimmed, then ignoring ASCII case.
- A smoothing hint on every parameter type (`with_smoothing`, `smoothing`,
  and `smoother` on `FloatParam`), reported as `smoothing_ms`; nothing
  else about the parameter changes.

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

- `vorbis`: an Ogg Vorbis decoder written from the Vorbis I specification
  and RFC 3533: floors of type 1, residues of types 0, 1 and 2, codebooks
  of every lookup type, channel coupling, both block sizes, 1 to 255
  channels handed out in the order of WAV, and the comments. A floor of
  type 0, which no encoder in use writes, is refused with an error that
  says so. Files libvorbis wrote decode to exactly the length encoded.
- `mp3`: an MP3 decoder written from ISO/IEC 11172-3 and 13818-3: layer
  III of MPEG-1, 2 and 2.5 at every sample rate, mono, dual channel,
  stereo and joint stereo with mid/side and intensity coding, long, short
  and mixed blocks, the bit reservoir and CRCs. ID3v2 tags are stepped
  over; a Xing, Info or VBRI header gives the length, and a LAME tag the
  samples the encoder added, which are removed, so that the audio decoded
  starts where the audio encoded did and has the length the tag states.
  Layers I and II and free format are refused.
- `AudioReader`, `decode` and `read_file` recognise both from the
  contents, and the reader seeks in both exactly: what is read after a
  seek is, bit for bit, what reading from the start gives there. In Ogg
  Vorbis by bisection over the pages, decoding two packets; in MP3 by
  counting the frames once, from their headers.
- `FileFormat` has two new variants, `OggVorbis` and `Mp3`, and
  `FormatError` two, `Vorbis` and `Mp3`: a `match` on either without a
  wildcard needs the new arms. Lossy files report 32 bits per sample and
  floating point.
- Both decoders run at a hundred times real time or more for stereo at
  44.1 kHz in a release build (`cargo run --release -p manguebyte-formats
  --example decode_speed`: about 340 times for Vorbis, 140 for MP3, on a
  busy machine).
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
  their source sample for sample; the lossy ones, against files libvorbis
  and LAME encoded from signals the tests make, must match their length
  exactly and their audio within measured bounds.

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
  handles memory: the queue, the triple buffer, the CLAP, VST 3 and LV2
  wrappers and the host. It also runs the window crate's
  platform-independent code and its headless backend.
- CI checks that no crate that processes audio depends on an interface
  crate (`scripts/check-layering.sh`), checks every dependency's licence
  with `cargo deny` against the list in `deny.toml`, and runs the window
  probe on Windows.
- CI runs `cargo mangue`'s end-to-end test on Windows, Linux and macOS:
  generate a plugin of each kind, build it, test it and bundle it, as CLAP,
  VST 3 and LV2.
- The block-size test of the facade also renders the song through a
  partitioned convolution in both latency modes, FIR filters in both
  modes, an expander and a phase-modulated wavetable oscillator, and
  requires identical output for every block pattern.
- CI builds `manguebyte-web` and `manguebyte-sampler` for WebAssembly,
  plays the sampler's module in Node.js and runs the engine's JavaScript
  tests against its module, failing if any test was skipped; Miri runs the
  native tests of the engine's WebAssembly exports; a job bundles both
  example plugins as LV2 and loads them with lilv; the Windows job runs the
  two-binaries test on the release bundles; and a check fails if two
  packages have an example of the same name, which cargo would build to
  one file.

### Fixed

- After a panic in the plugin, the CLAP and VST 3 wrappers did not give it
  the parameter values the host had sent in that block, so a plugin that
  follows events alone kept the old ones. The next block now starts with
  every parameter's value, as the LV2 wrapper's does.

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
