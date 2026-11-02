# ADR 0023: The MIDI output port of a standalone plugin

Status: accepted, 2026-11-01

## Context

ADR 0021 put a standalone plugin's audio devices and MIDI input ports
under a `DeviceManager`, chosen in `StandaloneSettings` and changed while
the plugin plays. The MIDI output for the notes a plugin sends
(`Plugin::NOTE_OUTPUT`) was left out: `Standalone::start_with_midi_output`
opened a port of the system directly with `MidiOutput::open`, wrote the
plugin's notes for it in the engine and sent them from a
`MidiOutputThread`. So the port could not be chosen or changed while the
plugin ran, was not saved with the settings, and could not run on the
headless backend, where every other part of the standalone is tested
(TECH_DEBT item 114).

The device manager already had a MIDI output port of its own: a
`DeviceSetup` names one (`midi_output`), `apply` and `follow` open and
close it with fallbacks, and `send_midi` writes to it from the program's
thread. Nothing could reach it from the audio thread.

What is needed:

- the port in the settings, by stable identifier, with the MPE option
  `MidiOutputOptions` had, saved as text that the settings of before
  still read;
- a change of port, or of MPE, while the plugin plays, without making the
  plugin again, reported like every other change, and hot-plug followed;
- ADR 0002 and 0003 kept: the audio thread hands the notes over without
  allocating, locking or waiting, and a change of port does not block it;
- no note left hanging on the port that is left.

JUCE's public documentation describes the analogue: `AudioDeviceManager`
keeps a default MIDI output (`setDefaultMidiOutputDevice`), the
standalone holder sends the processor's MIDI there, and the device
selector offers it as a list.

## Decision

**1. The port belongs to the device manager, and the audio thread reaches
it through a queue.** `DeviceManager::midi_output_transmitter` makes a
`midi_output_queue` and starts a thread that sends its packets, each when
it is due, to whatever port the manager has open then. The port lives in
a shared slot behind a mutex, which the sending thread and the program's
thread (`set_midi_output`, `apply`, `follow`, `send_midi`) lock; the audio
thread only pushes into the wait-free queue, so a change of port, or a
driver that hangs while sending, never reaches it. A packet due while no
port is open is dropped.

**2. A port that is left ends what it plays.** The slot follows every
message on its way to the port: the notes each port plays, by group,
channel and key, and its sustain pedals. When the port is changed or
closed (another port, none, unplugged, or the manager dropped), the old
port is first sent a note-off of velocity zero for every note it plays and
a controller 64 of zero for every pedal it holds down. A note-off for a
note the open port is not playing is not sent. So every note-on reaches
exactly one port, the one open when it was due, and that port receives
exactly one note-off for it: the plugin's own, or the one the change sent,
never both. Notes stamped before a change and due after it go to the new
port.

**3. The settings name the port and the MPE option.**
`StandaloneSettings::midi_output` is an `Option<DeviceRef>`, found by
identifier or by name like the input ports, and `midi_output_mpe` an
`Option<Zone>`, kept when the port is none. The text keeps version 1:
the port is the device setup's own `midi_output` key, and the zone a new
key, `midi_output_mpe=lower 15`, written only when set. Text written
before has neither and reads as no output; text written now reads, in a
program that does not know the keys, as its other settings. A plugin that
sends no notes opens no port and is offered none.

**4. The protocol follows the port and the MPE option, at the engine's
next block.** The engine writes the plugin's output with an
`OutputTranslator` on the audio thread, as before: MPE in the zone, or
plain MIDI 1.0, or MIDI 2.0 to a port that speaks it. When the protocol
must change, the program's thread makes a new translator (which
allocates) and hands it over a wait-free queue of one; the engine takes
it at the start of a block, ends the notes the old translator knows to be
sounding, in the old protocol, through the same queue, and sends the old
translator back over a second queue to be freed on the program's thread.
It takes one only when there is room to send the old one back, so it
never frees memory itself.

**5. The standalone reports it like the rest.** `set_midi_output` opens
the port through the manager without touching the audio;
`set_midi_output_mpe` changes the protocol; `apply_settings` and
`poll_devices` open, close and follow the port with the setup.
`SettingsReport::midi_output` names the open port, a missing one is a
`Fallback::MidiPortMissing` with the output direction, and `choices`
lists the output ports with the one in use marked. Following a MIDI port
event in the manager now reports the setup's MIDI fallbacks, so that a
port that goes away is reported, not only closed.

## Alternatives not taken

- **All notes off instead of note-offs.** Sending controller 123 on every
  channel is shorter, but a receiver may ignore it (MIDI leaves it
  optional under omni modes) and it ends notes the port did not get from
  this program. Explicit note-offs end exactly the notes it started.
- **Keeping the old port open until its notes end.** The plugin's later
  note-offs would reach the port that played the notes, as a hardware
  keyboard behaves. But the old port would stay open for as long as a
  note is held, possibly for ever, a port that went away could not be
  closed, and the routing of every message would depend on the note it
  belongs to.
- **Translating on the sending thread.** The engine would always write
  MIDI 2.0, and the sending thread would turn it into MIDI 1.0 or MPE for
  the port open at the time, so that a change of MPE needed no hand-over.
  But MIDI 1.0 the plugin sends itself would be translated up and down
  again, which is not lossless for controller sequences, and the MPE
  translation would no longer be the one the plugin wrappers use.
- **Locking the translator with `try_lock`.** Simple, but it is a lock on
  the audio thread (ADR 0002), and a failed `try_lock` would have to be
  retried every block.
- **Handing the transmitter to the callback through `DeviceIo`, as the
  MIDI input is.** It would put the output inside the hot-swap machinery,
  which parks and returns the callback with its MIDI receiver, for no
  gain: the transmitter has no state the manager needs back.
- **A new version of the settings' text.** Nothing in the old text
  changes meaning, and readers ignore keys they do not know, so a new
  version would only make older programs refuse newer settings.

## Consequences

- A standalone program can choose the MIDI output port and MPE in its
  settings screen (the ready-made view of ADR 0022 offers the port, for a
  plugin that sends notes), change them while the plugin plays and save them with
  the session; the port follows hot-plug, on the headless backend too.
  Tested there: a port chosen by identifier, a missing one falling back
  to none and opened when it appears, a port unplugged, a change of port
  and of MPE ending the notes on the old port, a sustain pedal lifted,
  notes sent from a device thread while the program changes ports and
  protocols, every note-on reaching one port with no note left playing,
  the audio going on while a port hangs, and no heap use on the audio
  path while the output changes.
- Breaking, unreleased: `Standalone::start_on` loses its `midi_output`
  argument (the port is in the options' settings), and `StandaloneSettings`,
  `SettingsReport` and `SettingsChoices` gain fields, so code building
  them with struct literals must add `midi_output`, `midi_output_mpe` and
  `midi_outputs`. `start_with_midi_output` stays, filling the settings.
  `midi_problems` now holds the reasons of the output's fallbacks as
  `MidiError::Connect`. The device manager's `send_midi` and
  `send_midi_packet` now leave out a note-off for a note the port is not
  playing, and `send_midi_packet` sends a port that speaks MIDI 2.0 the
  packet unchanged.
- The manager opens every output port as MIDI 1.0, so the MIDI 2.0 path
  is not reached yet, and nothing has run on a real port (TECH_DEBT item
  116).
