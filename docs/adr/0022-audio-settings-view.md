# ADR 0022: An audio settings view for a standalone program

Status: accepted, 2026-11-01

## Context

ADR 0021 gave a standalone plugin its device settings: what can be
chosen (`Standalone::choices`), a change of output, input, rate, buffer
size or MIDI input port while the plugin plays (the `set_` methods), a
report of what each change opened, and `poll_devices` to follow devices
that come and go. It left the screen that shows them for later (its
decision 5, TECH_DEBT item 114): every standalone program would have to
build its own from combo boxes and switches, and `manguebyte-ui`'s combo
box and switch could show only parameters. A device list is no
parameter: its items change while the program runs.

JUCE's public documentation describes the analogue,
`AudioDeviceSelectorComponent`: a component given an `AudioDeviceManager`
that lists device types, output and input devices, sample rates, buffer
sizes, MIDI inputs and outputs, and changes the manager directly. Its
`StandaloneFilterWindow` shows it in an options dialog.

What is needed:

- a view any standalone program can put in a floating panel (ADR 0019) or
  a window, with sections for the output device, the input device (left
  out for an instrument), the sample rate, the buffer size, the MIDI
  input ports and the last change's report;
- the keyboard, the wheel, menus and assistive technology as for every
  other control, and goldens like the rest of the toolkit;
- layering kept (ADR 0001 and 0007): no audio crate depends on an
  interface crate, and an interface build that does not ask for devices
  does not compile `cpal` and `midir`;
- state polled, nothing called back (ADR 0002 and 0003).

## Decision

**1. State-bound controls in `manguebyte-ui`.** `ComboBox::from_state`
lists a `State<Choices>`, the texts and the position chosen, and sets the
choice when the person picks; `Toggle::from_state` turns a `State<bool>`
over. They read the state once per frame, by its version, and share the
parameter-bound controls' drawing, keys, wheel, menus and description for
assistive technology; the parameter-bound constructors are unchanged. A
pick from a menu opened before the list changed is found again by its
text, and dropped if the item is gone, so a pick never lands on the
device that took its place. `Shown::First(n)` lets a container show the
first `n` of its children: a list whose length changes is built once as
slots and laid out with the spacing of only those shown, since the tree is
built once from a view and never rebuilt.

**2. The view takes a plain model and hands back requests.**
`manguebyte_ui::audio_settings` has an `AudioSettingsModel` of names,
identifiers and numbers (devices as `DeviceItem`, ports as
`MidiPortItem`, rates in hertz, buffers in frames, the report as text)
and an `AudioSettings`, a handle on the states the view's widgets share.
The program sets the model with `set_model` and polls
`take_requests`, which compares what the widgets hold with the model and
returns an `AudioSettingsRequest` for each section the person changed:
`Output(id)`, `Input(Option<id>)`, `SampleRate(hz)`, `BufferSize(frames)`,
`MidiInput { id, open }`. A request is taken into the model when it is
made, so it is asked for once; a pick not yet taken when a new model is
set is worked out against the model it was made on. A current value that
is missing from its list shows as nothing chosen. Rates are whole hertz,
as `StandaloneSettings::sample_rate` is. `AudioSettings` is `Send` and
`Sync`, so an editor spec can keep it and the glue can run on another
thread than the interface.

**3. The glue lives in `manguebyte-gui`, behind a `standalone` feature.**
`manguebyte_gui::audio_settings::sync(&mut standalone, &settings)` takes
the requests, carries each out with the standalone's `set_` methods
(`apply`), calls `poll_devices`, and, when anything changed or the model
is stale, builds the model again from `choices`, `settings` and `report`
(`model`, `model_from`). A quiet call lists no devices. The crate already
depends on `manguebyte-ui` and on audio crates (`manguebyte-plugin`,
`manguebyte-params`), and ADR 0001 lets an interface crate depend on an
audio crate. The feature is off by default, so plugin editors, which use
the same crate inside hosts, do not compile the device libraries; the
umbrella crate turns it on when both its `gui` and `standalone` features
are on.

## Alternatives not taken

- **A feature in `manguebyte-standalone` that depends on
  `manguebyte-ui`.** Small and next to the code it calls, but an audio
  crate would depend on an interface crate under a feature, which ADR
  0007 forbids and `scripts/check-layering.sh` (which resolves every
  feature) refuses.
- **The view in `manguebyte-ui` depending on `manguebyte-standalone`.**
  Every interface build would compile `cpal` and `midir` (ADR 0021,
  decision 5).
- **A crate of its own (`manguebyte-standalone-ui`).** Clean layering,
  but one more crate to publish for two functions, and a program that
  shows an editor in a window uses `manguebyte-gui` already.
- **The view changing a `Standalone` itself, as JUCE's component changes
  its manager.** It would need the standalone inside the interface, on
  the interface's thread, and a callback from each widget; a model and
  polled requests keep the view testable without devices and the
  standalone wherever the program keeps it.
- **Rebuilding the view when ports come and go.** The tree is built once
  from a view (ADR 0007); slots shown with `Shown::First` keep that, at
  the cost of a limit on how many ports are shown.

## Consequences

- A standalone program shows its devices, rate, buffer, MIDI ports and
  what the last change did with one view and one call from its timer.
  Tested in `manguebyte-ui` (requests, a model whose devices change, the
  keyboard, assistive technology, a floating panel, goldens in both
  themes at 1x and 2x) and in `manguebyte-gui` on the headless device
  backend (a pick reaching the standalone and its report coming back,
  MIDI ports, hot-plug, an effect's input).
- Not breaking for the parameter-bound controls. `Shown` has a new
  variant, which breaks a `match` on it with no wildcard arm.
- At most `MAX_MIDI_PORTS` (16) ports and `REPORT_LINES` (4) lines of
  the report are shown. There is no host (device type) section, no
  channel selection, no test tone and no CPU or
  latency readout; a buffer size the device chooses itself shows as
  "Default". A program calls `sync` from an application timer
  (`App::set_timer`, added by ADR 0024) or from a thread of its own
  (TECH_DEBT item 115).
