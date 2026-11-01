# ADR 0021: Device settings for a standalone plugin

Status: accepted, 2026-11-01

## Context

`manguebyte-standalone` ran a plugin on the default output device, opened
every MIDI input port it found, and moved the engine into the stream's
callback. Nothing could be chosen before it started beyond a
`StreamSettings` for the default device, and nothing could change while it
ran: another device, rate or buffer meant dropping the `Standalone`, which
made the plugin again and lost its state, its parameter handles and any
editor bound to them. Caecilia, an organ plugin that runs as a standalone
program and opens its editor with `Editor::open_in_app`, asked for what
JUCE standalones have: an audio and MIDI settings screen. Every standalone
needs it.

What is needed:

- options that name the output device (and the input, for an effect) by
  stable identifier, a sample rate, a buffer size and the MIDI input ports
  to open: all, none or a list; what cannot be had falls back, and the
  fallback is reported, not silent;
- while the plugin plays, from the program's thread: the choices (hosts,
  devices, the rates and buffer sizes each accepts, MIDI ports), and a
  change of any of them with the same plugin instance, and a report of
  what each change opened;
- a value to save with the session, as text, matching how `DeviceSetup` is
  saved;
- ADR 0002 and 0003 kept: the audio thread allocates, locks and waits for
  nothing, and state is polled.

`manguebyte-devices` already has most of it: `DeviceManager` opens a saved
`DeviceSetup` with fallbacks and a `RestoreReport`, follows devices as they
come and go, opens MIDI ports without touching the audio, and
`start_engine` runs a callback at a fixed `EngineFormat`, prepared once,
changing devices under it with a crossfade or a faded gap and a resampler
at the device's edge (the `hotswap` module).

The hard question is the sample rate. Under `start_engine` the engine's
rate is fixed and a device at another rate is resampled. A host does the
opposite: when the device's rate changes, the plugin is deactivated and
prepared again at the new rate, which costs a gap but no resampling.

JUCE's public documentation describes the analogue: `StandalonePluginHolder`
owns the processor and an `AudioDeviceManager`, saves the manager's state
as XML in the application's properties, and shows an
`AudioDeviceSelectorComponent` in its options dialog; the processor is
told the new rate through `prepareToPlay` when the device restarts.

## Decision

**1. The standalone runs its engine under a `DeviceManager`.** The engine
implements `AudioCallback`; `Standalone` keeps the manager and starts the
engine with `start_engine`, on the setup its settings stand for. Nothing
of the device machinery is written again in the standalone crate. The
engine reads the MIDI of the manager's ports through `DeviceIo`, after
the parameter announcements and the program's notes, so the order of
events is as before. `Standalone::start_on` takes any manager, which is
how the tests run on the headless backend.

**2. Settings are a value of their own, written in the device setup's
format.** `StandaloneSettings` holds what a settings screen changes: host,
output and input by `DeviceChoice` (an identifier with a name to fall back
to), sample rate, buffer size, channels, and `MidiInputs` (`All`, which
also takes ports plugged in later, or `Only` a list; `NONE` is the empty
list). It is written under its own header,
`manguebyte-standalone-settings 1`, followed by the lines of
`DeviceSetup`'s text and one more key, `midi_inputs=all|listed`. Reading
puts `DeviceSetup`'s header in front of the same lines and lets its parser
read them, so the escaping, the unknown keys and the line numbers of
errors are the device setup's. A header of its own keeps a
`DeviceSetup` reader from silently dropping the MIDI mode, and the other
way round. The program saves `Standalone::settings()`, which names what
was asked for rather than what a fallback put in its place, and hands it
back in `StandaloneOptions::settings`.

**3. The rate follows the device by default, and the caller can fix it.**
`RatePolicy::FollowDevice`, the default, does what hosts do: before a
change, the standalone asks the manager what the setup would open
(`DeviceManager::preview`, new); when that runs at another rate than the
plugin, it applies the setup with a new engine format
(`DeviceManager::apply_with_format`, new), which fades the old device out,
stops it, releases the engine, prepares it again with the new format and
starts the new device, fading in. `Engine::prepare_for` calls the plugin's
`prepare` again only when the rate or the largest block changed, and the
transport keeps its place in beats. If what opened still differs from the
plan (a driver that could not be described while another ran, another
host), the format is corrected once more after the fact.
`RatePolicy::Fixed(rate)` keeps the plugin at `rate`, prepared once: the
devices are asked for `rate` unless the settings name another (a device
that cannot run at it is not reported as a fallback, since nobody asked
the device for it), a device at another rate is fed through the edge's
resampler, under a millisecond of latency, and a change of device crosses
over without a gap where the backend can run two streams.

The engine format also carries the output device's channel count. A
change of channel count goes through `apply_with_format` too, without
preparing the plugin, so that auxiliary buses keep landing on the device
channels they are routed to (ADR 0017) instead of being folded by the
edge's channel mapping. The largest block is
`StandaloneOptions::block_size` and never changes with the hardware
buffer: the edge cuts a longer buffer into blocks.

**4. Every change returns a report.** `SettingsReport` says which devices
play, at what rate, with what buffer and channels, the plugin's rate and
whether it was prepared again, the open MIDI ports, every `Fallback`, the
`SwapReport` of the device change, and an error. A change that fails is
undone: the earlier settings are opened again, and the report's error says
what failed. `Standalone::report` keeps the last one; its `Display` is a
few lines a settings screen can show. MIDI ports open and close through
the manager's `enable_midi_input` and `disable_midi_input`, without
touching the audio. `Standalone::poll_devices`, from the program's timer,
follows hot-plug through `DeviceManager::follow`, opens new ports under
`MidiInputs::All`, and corrects the format if a device that took over runs
at another rate.

**5. The ready-made settings view waits for state-bound widgets.**
`manguebyte-ui` may depend on audio crates (ADR 0001 forbids only the
other direction), but a view that depended on `manguebyte-standalone`
would bring `cpal` and `midir` into every interface build. The view
should take a plain data model, built from `SettingsChoices` and
`SettingsReport` by the program or by glue in an interface crate, and
hand back requests the program polls. The widgets it needs do not exist
yet: the combo box and the toggle bind only parameters. It is left for
another change (TECH_DEBT item 114).

## Alternatives not taken

- **Always resample (`start_engine` alone).** The plugin would be prepared
  once and device changes would never leave a gap. But a plugin designed
  for its host's rate would run at a rate the user did not choose, with a
  resampler's latency and colour on every device that differs, and a
  standalone would behave unlike the same plugin in a host. Offered as
  `RatePolicy::Fixed`.
- **Always restart (`DeviceManager::start` and its per-device `prepare`).**
  Simple and like a host, but every change, even of device at the same
  rate, would stop the stream with no fade, and the crossfade machinery
  would be lost.
- **Taking the engine back with `take_callback` and starting again.** It
  works, but the callback comes back as a `Box<dyn AudioCallback>`, which
  would need to be boxed again on every change, and the stream would stop
  without a fade. `apply_with_format` keeps the engine inside the manager
  and fades.
- **Deciding the rate after opening only.** Open the setup, read the
  running rate, and reopen at it if needed. It needs no preview, but a
  change of rate would open the new device twice. The preview decides
  first; the check after opening stays as a safety net.
- **`DeviceSetup` itself as the saved value.** It cannot say "every MIDI
  port, including later ones", and it names a MIDI output the standalone
  does not use (the plugin's notes go out through `MidiOutputOptions` and a
  timed thread).
- **A view in `manguebyte-ui` depending on `manguebyte-standalone`.** See
  decision 5.

## Consequences

- A standalone program can show and change its devices while the plugin
  plays; the plugin, its parameters, state and editor survive every
  change. Tested on the headless backend: options, fallbacks, a change of
  output at the same and at another rate under both policies, MIDI ports
  opened and closed while playing, hot-plug, a failed change undone, a
  save and restore, and no heap use on the audio path before and after a
  change.
- Breaking for programs using `StandaloneOptions`: `output` and
  `open_midi_inputs` moved into `settings` (`sample_rate`, `buffer_size`,
  `output_channels`; `MidiInputs::NONE`), and the options are no longer
  `Copy`. MIDI input ports that cannot be opened are fallbacks in the
  report instead of entries in `midi_problems`, which now holds the MIDI
  output's problems. With no output device, `start` succeeds and waits for
  one instead of failing.
- The MIDI output for a plugin's notes is still opened directly on the
  system's ports, outside the manager, and cannot be changed while it
  runs.
- No real device has been switched under a running standalone yet; the
  crossfade itself has only run on the headless backend (TECH_DEBT item
  114).
