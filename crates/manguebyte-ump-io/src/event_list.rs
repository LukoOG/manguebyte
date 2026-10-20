//! CoreMIDI's `MIDIEventList`, laid out in Rust.
//!
//! Apple's CoreMIDI reference defines the list as a header followed by
//! packets of variable length, every field 32 bits wide or made of 32-bit
//! halves, packed to four bytes (`#pragma pack(push, 4)` in
//! `MIDIServices.h`):
//!
//! ```text
//! MIDIEventList   protocol    MIDIProtocolID (SInt32)   1 word
//!                 numPackets  UInt32                    1 word
//!                 packet[]    MIDIEventPacket, numPackets of them
//! MIDIEventPacket timeStamp   MIDITimeStamp (UInt64)    2 words, native order
//!                 wordCount   UInt32                    1 word
//!                 words[]     UInt32, wordCount of them, at most 64
//! ```
//!
//! The next packet starts right after the last word of the one before
//! (what `MIDIEventPacketNext` computes), so the whole list is an array of
//! native-endian `u32`, which is how [`EventList`] holds it.
//! `MIDISendEventList` reads it from [`EventList::as_words`].
//!
//! A packet holds whole Universal MIDI Packets that share a time stamp;
//! [`EventList::push`] adds a message to the last packet when its time
//! stamp is the same and there is room, and starts a new packet otherwise,
//! as `MIDIEventListAdd` does. A message is never split between packets.
//!
//! After [`EventList::new`], which allocates, nothing allocates.

/// `kMIDIProtocol_1_0`.
pub const PROTOCOL_1_0: i32 = 1;
/// `kMIDIProtocol_2_0`.
pub const PROTOCOL_2_0: i32 = 2;

/// The most words one `MIDIEventPacket` carries: its `words` array is 64
/// long.
pub const PACKET_WORDS: usize = 64;

/// The list header: `protocol` and `numPackets`.
const LIST_HEADER: usize = 2;
/// A packet header: `timeStamp` (two words) and `wordCount`.
const PACKET_HEADER: usize = 3;

/// The default room of a list, in words: 4 KiB, which is fifteen full
/// packets.
pub const DEFAULT_CAPACITY: usize = 1_024;

/// A `MIDIEventList` being filled, with room for a fixed number of words.
#[derive(Debug, Clone)]
pub struct EventList {
    /// The list as CoreMIDI reads it. Never longer than its capacity, so
    /// it never reallocates.
    words: Vec<u32>,
    /// Where the last packet's header starts, if there is a packet.
    current: Option<usize>,
    current_timestamp: u64,
}

impl EventList {
    /// An empty list in `protocol` ([`PROTOCOL_1_0`] or
    /// [`PROTOCOL_2_0`]), with room for `capacity` words, header included.
    /// The room is at least one full packet. Allocates.
    pub fn new(protocol: i32, capacity: usize) -> Self {
        let capacity = capacity.max(LIST_HEADER + PACKET_HEADER + PACKET_WORDS);
        let mut words = Vec::with_capacity(capacity);
        words.extend_from_slice(&[protocol as u32, 0]);
        Self {
            words,
            current: None,
            current_timestamp: 0,
        }
    }

    /// The list's protocol.
    pub fn protocol(&self) -> i32 {
        self.words[0] as i32
    }

    /// How many `MIDIEventPacket`s the list holds.
    pub fn len(&self) -> usize {
        self.words[1] as usize
    }

    /// Whether the list holds no packet.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The room of the list, in words, header included.
    pub fn capacity(&self) -> usize {
        self.words.capacity()
    }

    /// Empties the list, keeping its protocol and its room.
    pub fn clear(&mut self) {
        self.words.truncate(LIST_HEADER);
        self.words[1] = 0;
        self.current = None;
    }

    /// Adds one whole Universal MIDI Packet, `message`, to be sent at
    /// `timestamp` (CoreMIDI's host time; zero is now).
    ///
    /// Returns `false`, adding nothing, if the list has no room left for
    /// it, or if `message` is not one whole packet (see
    /// [`is_one_packet`](crate::is_one_packet)). Send the list and
    /// [`clear`](Self::clear) it, then push again.
    pub fn push(&mut self, timestamp: u64, message: &[u32]) -> bool {
        if !crate::is_one_packet(message) {
            return false;
        }
        let room = self.words.capacity() - self.words.len();
        if let Some(header) = self.current {
            let count = self.words[header + 2] as usize;
            if self.current_timestamp == timestamp
                && count + message.len() <= PACKET_WORDS
                && message.len() <= room
            {
                self.words.extend_from_slice(message);
                self.words[header + 2] = (count + message.len()) as u32;
                return true;
            }
        }
        if PACKET_HEADER + message.len() > room {
            return false;
        }
        let header = self.words.len();
        let [a, b, c, d, e, f, g, h] = timestamp.to_ne_bytes();
        self.words.extend_from_slice(&[
            u32::from_ne_bytes([a, b, c, d]),
            u32::from_ne_bytes([e, f, g, h]),
            message.len() as u32,
        ]);
        self.words.extend_from_slice(message);
        self.words[1] += 1;
        self.current = Some(header);
        self.current_timestamp = timestamp;
        true
    }

    /// The list as `MIDISendEventList` reads it: a `MIDIEventList`, four
    /// bytes aligned.
    pub fn as_words(&self) -> &[u32] {
        &self.words
    }

    /// The packets of the list, read back: each one's time stamp and
    /// words.
    pub fn packets(&self) -> impl Iterator<Item = (u64, &[u32])> + '_ {
        let mut at = LIST_HEADER;
        (0..self.len()).map(move |_| {
            let [a, b, c, d] = self.words[at].to_ne_bytes();
            let [e, f, g, h] = self.words[at + 1].to_ne_bytes();
            let timestamp = u64::from_ne_bytes([a, b, c, d, e, f, g, h]);
            let count = self.words[at + 2] as usize;
            let words = &self.words[at + PACKET_HEADER..at + PACKET_HEADER + count];
            at += PACKET_HEADER + count;
            (timestamp, words)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A MIDI 2.0 note-on, group 0, channel 0, note 60, velocity 0x2000.
    const NOTE_ON: [u32; 2] = [0x4090_3C00, 0x2000_0000];
    /// A MIDI 2.0 registered per-note controller 74 on note 60.
    const PER_NOTE: [u32; 2] = [0x4000_3C4A, 0x8000_0000];

    #[test]
    fn the_layout_is_the_header_then_packets_of_their_own_length() {
        let mut list = EventList::new(PROTOCOL_2_0, 64);
        assert!(list.push(0, &NOTE_ON));
        assert!(list.push(0, &PER_NOTE));
        let at = 0x0000_0001_0000_0002_u64;
        assert!(list.push(at, &[0x1000_F800]));
        let stamp = at.to_ne_bytes();
        let low = u32::from_ne_bytes([stamp[0], stamp[1], stamp[2], stamp[3]]);
        let high = u32::from_ne_bytes([stamp[4], stamp[5], stamp[6], stamp[7]]);
        assert_eq!(
            list.as_words(),
            [
                2, // protocol: kMIDIProtocol_2_0
                2, // numPackets
                0,
                0,
                4, // timeStamp 0, wordCount 4
                NOTE_ON[0],
                NOTE_ON[1],
                PER_NOTE[0],
                PER_NOTE[1],
                low,
                high,
                1, // timeStamp, wordCount 1
                0x1000_F800,
            ]
        );
        assert_eq!(
            list.packets().collect::<Vec<_>>(),
            [
                (0, &[NOTE_ON[0], NOTE_ON[1], PER_NOTE[0], PER_NOTE[1]][..]),
                (at, &[0x1000_F800][..]),
            ]
        );
        assert_eq!((list.protocol(), list.len()), (PROTOCOL_2_0, 2));
    }

    #[test]
    fn a_packet_never_holds_more_than_sixty_four_words() {
        let mut list = EventList::new(PROTOCOL_2_0, DEFAULT_CAPACITY);
        let stream = [0xF000_0000, 1, 2, 3];
        for _ in 0..17 {
            assert!(list.push(0, &stream));
        }
        let sizes: Vec<usize> = list.packets().map(|(_, words)| words.len()).collect();
        assert_eq!(sizes, [64, 4]);
    }

    #[test]
    fn a_full_list_refuses_and_takes_again_once_cleared() {
        // The smallest list: one packet of 64 words.
        let mut list = EventList::new(PROTOCOL_2_0, 0);
        let capacity = list.capacity();
        let pointer = list.as_words().as_ptr();
        for _ in 0..32 {
            assert!(list.push(0, &NOTE_ON));
        }
        assert!(!list.push(0, &NOTE_ON));
        // Another time stamp needs a new header, which has no room either.
        assert!(!list.push(1, &[0x1000_F800]));
        assert_eq!(list.len(), 1);
        list.clear();
        assert!(list.is_empty());
        assert!(list.push(1, &[0x1000_F800]));
        // Never reallocated.
        assert_eq!(
            (list.capacity(), list.as_words().as_ptr()),
            (capacity, pointer)
        );
    }

    #[test]
    fn what_is_not_one_whole_packet_is_refused() {
        let mut list = EventList::new(PROTOCOL_1_0, 64);
        assert!(!list.push(0, &[]));
        assert!(!list.push(0, &NOTE_ON[..1]));
        assert!(!list.push(0, &[0x2090_3C64, 0x2080_3C00]));
        assert!(list.is_empty());
        assert_eq!(list.as_words(), [1, 0]);
    }
}
