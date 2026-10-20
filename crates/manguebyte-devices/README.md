# manguebyte-devices

Audio and MIDI device input and output for the Manguebyte Audio Framework.

Part of the [Manguebyte Audio Framework](https://github.com/alessonqueirozdev-hub/manguebyte). Most projects depend on
[`manguebyte`](https://crates.io/crates/manguebyte), which gathers every part
under one name; depend on this crate directly to use only this part.

It lists and opens audio devices on the system's audio hosts (WASAPI,
CoreAudio, ALSA, AAudio, and JACK behind the `jack` feature), runs
callbacks on planar `f32` blocks, follows the drift between two devices'
clocks, follows devices as they come and go (by polling, as no backend
notifies), restores saved setups with fallbacks, and opens MIDI ports.
Its `DeviceManager` can also run an engine at a fixed format and change
the devices under it in the middle of a song, without stopping or
preparing it again: a crossfade where two devices can play at once, a
short faded gap where they cannot (`DeviceManager::start_engine`, the
`hotswap` module). The crossfade is proven on the headless backend; two
real outputs have not crossfaded yet.

MIDI goes out from the audio thread too: `midi::midi_output_queue`
stamps each Universal MIDI Packet with the moment its frame is heard,
and `MidiOutput::spawn` sends it then. A port opened with
`MidiOutput::open_with_protocol(id, MidiProtocol::Midi2)` that speaks
MIDI 2.0 is sent the packets unchanged, through the system's own
Universal MIDI Packet output: CoreMIDI's `MIDISendEventList` on macOS
(11 or newer) and iOS (14 or newer), the ALSA sequencer's UMP events on
Linux (alsa-lib 1.2.10 and Linux 6.5 or newer), reached through the
workspace crate [`manguebyte-ump-io`](../manguebyte-ump-io). Every other
port speaks MIDI 1.0 bytes, MIDI 2.0 going out by the default
translation, and says why (`MidiOutput::fallback`): so does every port
on Windows, where Windows MIDI Services' own API is not reachable from
the `windows` crate. `midi_output_ports_with_protocols` lists the ports
with the protocol each would speak; the example `midi2_output` prints
them.

`AudioBackend` is a public seam through which a backend from another
crate runs behind the device manager. The ASIO crate
[`manguebyte-pro-io`](../manguebyte-pro-io), opt-in and under the GPLv3,
is one; closed-source programs reach ASIO through `cpal`'s own `asio`
feature instead, as the documentation of `audio_hosts` explains.

Documentation: <https://docs.rs/manguebyte-devices>

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
