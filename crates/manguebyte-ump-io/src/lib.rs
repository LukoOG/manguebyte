//! The systems' own Universal MIDI Packet output, for `manguebyte-devices`.
//!
//! `manguebyte-devices` forbids `unsafe` code, and the systems' UMP calls
//! are C functions, so they live here, each with the reason it is sound
//! written beside it. Applications use them through
//! `manguebyte_devices::midi::MidiOutput::open_with_protocol`; this crate
//! is its plumbing.
//!
//! - [`event_list`] lays out CoreMIDI's `MIDIEventList` in Rust, and
//!   [`seq_event`] the ALSA sequencer's `snd_seq_ump_event_t`. Both are
//!   plain data, compiled and tested on every system, so the bytes each
//!   system is handed are checked even where it cannot run.
//! - `coremidi` (macOS and iOS) sends event lists in the MIDI 2.0
//!   protocol with `MIDISendEventList`, and reads which protocol a
//!   destination speaks from `kMIDIPropertyProtocolID`.
//! - `alsa` (Linux) opens a sequencer client in the MIDI 2.0 protocol and
//!   sends UMP events with `snd_seq_ump_event_output_direct`, and reads
//!   which protocol a client speaks.
//! - `windows` says whether Windows MIDI Services is installed. It sends
//!   nothing: see its documentation for why.
//!
//! # Run-time detection
//!
//! Nothing is linked at build time. CoreMIDI and `libasound.so.2` are
//! opened with `dlopen` the first time they are needed and every function
//! is looked up with `dlsym`, so a system without the UMP calls still runs
//! the program, and the caller is told what is missing: macOS before 11 or
//! iOS before 14 (no `MIDISendEventList`), alsa-lib before 1.2.10 (no
//! `snd_seq_ump_event_output_direct`), a kernel before 6.5 (the sequencer
//! refuses a MIDI 2.0 client), or no ALSA at all.
//!
//! # Sources
//!
//! Written from the MIDI Association's "Universal MIDI Packet (UMP)
//! Format and MIDI 2.0 Protocol" (M2-104-UM, version 1.1.1), Apple's
//! CoreMIDI reference (`MIDIEventList`, `MIDIEventPacket`,
//! `MIDISendEventList`, `MIDIProtocolID`, `kMIDIPropertyProtocolID`), the
//! alsa-lib sequencer documentation (the UMP additions of alsa-lib
//! 1.2.10 and the sequencer's UMP support in Linux 6.5) and Microsoft's
//! Windows MIDI Services documentation.

pub mod event_list;
pub mod seq_event;

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "ios"))]
mod dl;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod coremidi;

#[cfg(target_os = "linux")]
pub mod alsa;

#[cfg(windows)]
pub mod windows;

/// How many words a Universal MIDI Packet of this message type occupies
/// (M2-104-UM, table 4). Only the low four bits are used.
pub const fn words_for_type(message_type: u8) -> usize {
    match message_type & 0x0F {
        0x0 | 0x1 | 0x2 | 0x6 | 0x7 => 1,
        0x3 | 0x4 | 0x8 | 0x9 | 0xA => 2,
        0xB | 0xC => 3,
        _ => 4,
    }
}

/// Whether `message` is exactly one whole Universal MIDI Packet: as many
/// words as its first word's message type says.
pub fn is_one_packet(message: &[u32]) -> bool {
    match message.first() {
        Some(first) => message.len() == words_for_type((first >> 28) as u8),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_packet_is_as_long_as_its_message_type_says() {
        assert!(is_one_packet(&[0x2090_3C64]));
        assert!(is_one_packet(&[0x4090_3C00, 0x8000_0000]));
        assert!(is_one_packet(&[0xF000_0000, 0, 0, 0]));
        assert!(!is_one_packet(&[]));
        assert!(!is_one_packet(&[0x4090_3C00]));
        assert!(!is_one_packet(&[0x2090_3C64, 0x2080_3C00]));
    }
}
