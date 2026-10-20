# manguebyte-ump-io

The systems' own Universal MIDI Packet output, for `manguebyte-devices`.

Part of the [Manguebyte Audio Framework](https://github.com/alessonqueirozdev-hub/manguebyte).
Applications do not use this crate directly: they open a MIDI 2.0 port
with `manguebyte_devices::midi::MidiOutput::open_with_protocol`, which
uses it. `manguebyte-devices` forbids `unsafe` code, so the foreign calls
live here, each with the reason it is sound written beside it.

- **macOS and iOS:** CoreMIDI's `MIDISendEventList`, with event lists in
  the MIDI 2.0 protocol (`kMIDIProtocol_2_0`), to destinations whose
  `kMIDIPropertyProtocolID` says MIDI 2.0. Needs macOS 11 or iOS 14.
- **Linux:** the ALSA sequencer's UMP events, from a client in the MIDI
  2.0 protocol to a port of a MIDI 2.0 client. Needs alsa-lib 1.2.10 and
  Linux 6.5.
- **Windows:** nothing is sent. Windows MIDI Services' API is a WinRT
  component the `windows` crate does not project; the crate only says
  whether the service and its App SDK runtime are installed.

CoreMIDI and `libasound.so.2` are opened at run time with `dlopen`, so
nothing is linked at build time and a system without the UMP calls still
runs the program, which falls back to MIDI 1.0 and says what is missing.
CoreMIDI's `MIDIEventList` and the sequencer's `snd_seq_ump_event_t` are
laid out in Rust and tested on every system.

Written from the MIDI Association's UMP and MIDI 2.0 Protocol
specification (M2-104-UM), Apple's CoreMIDI reference, the alsa-lib
sequencer documentation and Microsoft's Windows MIDI Services
documentation.

Documentation: <https://docs.rs/manguebyte-ump-io>

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
