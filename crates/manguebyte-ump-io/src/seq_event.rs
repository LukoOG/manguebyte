//! The ALSA sequencer's `snd_seq_ump_event_t`, laid out in Rust.
//!
//! alsa-lib 1.2.10 and Linux 6.5 added UMP events to the sequencer. A UMP
//! event has the header of every sequencer event and, in place of the
//! twelve bytes of event data, the four words of one Universal MIDI
//! Packet:
//!
//! ```text
//! offset  field   type
//!      0  type    snd_seq_event_type_t (unsigned char)
//!      1  flags   unsigned char: SND_SEQ_EVENT_UMP and the time flags
//!      2  tag     unsigned char
//!      3  queue   unsigned char
//!      4  time    snd_seq_timestamp_t (a tick or seconds and nanoseconds)
//!     12  source  snd_seq_addr_t (client, port)
//!     14  dest    snd_seq_addr_t (client, port)
//!     16  ump     unsigned int[4]
//!     32
//! ```
//!
//! [`SeqUmpEvent`] has that layout, which the compile-time checks below
//! hold it to, so a pointer to one is what
//! `snd_seq_ump_event_output_direct` takes.

use std::mem::{offset_of, size_of};

/// `SND_SEQ_EVENT_UMP`: the event carries a Universal MIDI Packet.
pub const EVENT_UMP: u8 = 1 << 5;
/// `SND_SEQ_QUEUE_DIRECT`: delivered at once, without a queue.
pub const QUEUE_DIRECT: u8 = 253;
/// `SND_SEQ_ADDRESS_SUBSCRIBERS`: to every port subscribed to the source.
pub const ADDRESS_SUBSCRIBERS: u8 = 254;
/// `SND_SEQ_ADDRESS_UNKNOWN`.
pub const ADDRESS_UNKNOWN: u8 = 253;

/// `SND_SEQ_CLIENT_LEGACY_MIDI`: a client of MIDI 1.0 events, not UMP.
pub const CLIENT_LEGACY_MIDI: i32 = 0;
/// `SND_SEQ_CLIENT_UMP_MIDI_1_0`: a UMP client in the MIDI 1.0 protocol.
pub const CLIENT_UMP_MIDI_1_0: i32 = 1;
/// `SND_SEQ_CLIENT_UMP_MIDI_2_0`: a UMP client in the MIDI 2.0 protocol.
pub const CLIENT_UMP_MIDI_2_0: i32 = 2;

/// `snd_seq_addr_t`: a client and one of its ports.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SeqAddr {
    /// The client number.
    pub client: u8,
    /// The port number within the client.
    pub port: u8,
}

/// `snd_seq_ump_event_t`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SeqUmpEvent {
    /// `type`. Not read for a UMP event, whose packet says what it is;
    /// left at zero.
    pub kind: u8,
    /// `flags`: [`EVENT_UMP`], and zero for a tick time stamp, absolute.
    pub flags: u8,
    /// `tag`.
    pub tag: u8,
    /// `queue`: [`QUEUE_DIRECT`] for an event sent at once.
    pub queue: u8,
    /// `time`, unused for an event sent at once.
    pub time: [u32; 2],
    /// `source`: the sending port. The sequencer fills in the client.
    pub source: SeqAddr,
    /// `dest`.
    pub dest: SeqAddr,
    /// `ump`: one Universal MIDI Packet, its unused words zero.
    pub ump: [u32; 4],
}

const _: () = {
    assert!(size_of::<SeqUmpEvent>() == 32);
    assert!(std::mem::align_of::<SeqUmpEvent>() == 4);
    assert!(offset_of!(SeqUmpEvent, flags) == 1);
    assert!(offset_of!(SeqUmpEvent, queue) == 3);
    assert!(offset_of!(SeqUmpEvent, time) == 4);
    assert!(offset_of!(SeqUmpEvent, source) == 12);
    assert!(offset_of!(SeqUmpEvent, dest) == 14);
    assert!(offset_of!(SeqUmpEvent, ump) == 16);
};

impl SeqUmpEvent {
    /// An event that carries `message`, one whole Universal MIDI Packet,
    /// from `port` to every port subscribed to it, at once: what a
    /// sequencer output sends. `None` if `message` is not one whole
    /// packet (see [`is_one_packet`](crate::is_one_packet)).
    pub fn to_subscribers(port: u8, message: &[u32]) -> Option<Self> {
        if !crate::is_one_packet(message) {
            return None;
        }
        let mut ump = [0; 4];
        ump[..message.len()].copy_from_slice(message);
        Some(Self {
            flags: EVENT_UMP,
            queue: QUEUE_DIRECT,
            source: SeqAddr { client: 0, port },
            dest: SeqAddr {
                client: ADDRESS_SUBSCRIBERS,
                port: ADDRESS_UNKNOWN,
            },
            ump,
            ..Self::default()
        })
    }

    /// Whether the event carries a Universal MIDI Packet.
    pub fn is_ump(&self) -> bool {
        self.flags & EVENT_UMP != 0
    }

    /// The packet the event carries: as many of its words as the message
    /// type says. Empty if the event is not a UMP event.
    pub fn words(&self) -> &[u32] {
        if !self.is_ump() {
            return &[];
        }
        &self.ump[..crate::words_for_type((self.ump[0] >> 28) as u8)]
    }
}

/// Reads a sequencer port's address as `midir` writes it, `client:port`.
pub fn parse_address(id: &str) -> Option<(u8, u8)> {
    let (client, port) = id.split_once(':')?;
    Some((client.trim().parse().ok()?, port.trim().parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The event's bytes, as the sequencer reads them.
    fn bytes(event: &SeqUmpEvent) -> Vec<u8> {
        let mut out = vec![event.kind, event.flags, event.tag, event.queue];
        for word in event.time {
            out.extend_from_slice(&word.to_ne_bytes());
        }
        out.extend_from_slice(&[
            event.source.client,
            event.source.port,
            event.dest.client,
            event.dest.port,
        ]);
        for word in event.ump {
            out.extend_from_slice(&word.to_ne_bytes());
        }
        out
    }

    #[test]
    fn an_event_to_subscribers_carries_the_packet_unchanged() {
        let note_on = [0x4190_3C00, 0x2000_0000];
        let event = SeqUmpEvent::to_subscribers(3, &note_on).unwrap();
        let mut expected = vec![0, EVENT_UMP, 0, QUEUE_DIRECT, 0, 0, 0, 0, 0, 0, 0, 0];
        expected.extend_from_slice(&[0, 3, ADDRESS_SUBSCRIBERS, ADDRESS_UNKNOWN]);
        expected.extend_from_slice(&note_on[0].to_ne_bytes());
        expected.extend_from_slice(&note_on[1].to_ne_bytes());
        expected.extend_from_slice(&[0; 8]);
        assert_eq!(bytes(&event), expected);
        assert_eq!(bytes(&event).len(), size_of::<SeqUmpEvent>());
        assert!(event.is_ump());
        assert_eq!(event.words(), note_on);
    }

    #[test]
    fn every_size_of_packet_fits_and_comes_back() {
        for message in [
            &[0x2090_3C64][..],
            &[0x4000_3C4A, 0x8000_0000],
            &[0xB000_0000, 1, 2],
            &[0xF000_0000, 1, 2, 3],
        ] {
            let event = SeqUmpEvent::to_subscribers(0, message).unwrap();
            assert_eq!(event.words(), message);
        }
        assert_eq!(SeqUmpEvent::to_subscribers(0, &[]), None);
        assert_eq!(SeqUmpEvent::to_subscribers(0, &[0x4000_3C4A]), None);
        assert!(SeqUmpEvent::default().words().is_empty());
    }

    #[test]
    fn port_addresses_are_read_as_midir_writes_them() {
        assert_eq!(parse_address("128:0"), Some((128, 0)));
        assert_eq!(parse_address("20:1"), Some((20, 1)));
        assert_eq!(parse_address("name:Synth"), None);
        assert_eq!(parse_address("128"), None);
        assert_eq!(parse_address("300:0"), None);
    }
}
