//! The Universal MIDI Packet (UMP) and the MIDI 2.0 messages it carries.
//!
//! A UMP stream is a sequence of 32-bit words. Each message occupies one to
//! four of them, and the top four bits of the first word (the message type)
//! say how many. This module reads and writes those packets and gives typed
//! access to every message the specification defines:
//!
//! - [`Packet`]: one packet as raw words, read from and written to slices.
//! - [`Message`]: what a packet means. Utility messages, system messages,
//!   MIDI 1.0 and MIDI 2.0 channel voice messages, 7-bit and 8-bit system
//!   exclusive data, mixed data sets, flex data and UMP stream messages.
//! - [`packets`]: iterates over the packets in a slice of words.
//! - [`sysex7_packets`], [`sysex8_packets`], [`text_packets`] and the
//!   name functions cut a long payload into packets; [`Assembler`] puts one
//!   back together.
//!
//! Written from "Universal MIDI Packet (UMP) Format and MIDI 2.0 Protocol",
//! version 1.1.1 (M2-104-UM, the MIDI Association and AMEI). Section,
//! figure and table numbers in comments refer to that document.
//!
//! Nothing here allocates except [`Assembler::new`], so packets can be read,
//! written and converted on the audio thread.
//!
//! ```
//! use manguebyte_midi::ump::{ChannelVoice2, Message, NoteAttribute, Packet};
//!
//! let note_on = Message::Midi2 {
//!     group: 0,
//!     message: ChannelVoice2::NoteOn {
//!         channel: 2,
//!         note: 60,
//!         velocity: 0xC000,
//!         attribute: NoteAttribute::NONE,
//!     },
//! };
//! let packet = note_on.to_packet();
//! assert_eq!(packet.words(), &[0x4092_3C00, 0xC000_0000]);
//!
//! // Reading: the message type decides how many words belong to the packet.
//! let stream = [0x4092_3C00, 0xC000_0000, 0x10F8_0000];
//! let first = Packet::from_words(&stream).unwrap();
//! assert_eq!(first.word_count(), 2);
//! assert_eq!(first.message(), Ok(note_on));
//! ```
//!
//! # Limits
//!
//! Reserved bits are ignored when reading, as the specification requires of
//! receivers, and written as zero. Reading a packet and writing it again
//! therefore gives the same words only if the sender left reserved bits at
//! zero. One further normalisation is deliberate: a MIDI 1.0 note-on with
//! velocity zero reads as a note-off, like everywhere else in this crate.
//!
//! Messages and message types that the specification reserves are reported
//! as errors by [`Packet::message`]; the packet itself is still valid and
//! can be skipped or passed on.

use core::fmt;

use crate::{MidiMessage, SystemMessage};

mod flex;
mod stream;

pub use flex::{
    Alteration, ChordName, FlexAddress, FlexData, FlexMessage, FlexText, Metronome, TextKind,
    chord_type, text_packets,
};
pub use stream::{
    DeviceIdentity, EndpointInfo, FunctionBlockInfo, StreamConfiguration, StreamMessage,
    StreamText, endpoint_filter, endpoint_name_packets, function_block_filter,
    function_block_name_packets, product_instance_id_packets, protocol,
};

/// Why words could not be read as a packet, or a packet as a message.
///
/// More reasons may be added, so a `match` on this type needs a wildcard
/// arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum UmpError {
    /// The words end before the packet does.
    Truncated {
        /// How many words the packet occupies.
        needed: usize,
        /// How many words were there.
        available: usize,
    },
    /// The output slice is too short for the packet.
    NoRoom {
        /// How many words the packet occupies.
        needed: usize,
        /// How many words the output could take.
        available: usize,
    },
    /// The message type is reserved by the specification. The packet's
    /// size is still known, so a reader can skip it.
    ReservedMessageType(u8),
    /// The message type is defined but this status within it is not.
    UnknownStatus {
        /// The message type, `0..=15`.
        message_type: u8,
        /// The status or opcode field, as wide as that message type makes
        /// it.
        status: u16,
    },
    /// A field holds a value the format does not allow.
    Invalid(&'static str),
}

impl fmt::Display for UmpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Truncated { needed, available } => write!(
                f,
                "packet needs {needed} words but only {available} are left"
            ),
            Self::NoRoom { needed, available } => write!(
                f,
                "packet needs {needed} words but the output holds {available}"
            ),
            Self::ReservedMessageType(message_type) => {
                write!(f, "message type {message_type:#X} is reserved")
            }
            Self::UnknownStatus {
                message_type,
                status,
            } => write!(
                f,
                "status {status:#X} is not defined for message type {message_type:#X}"
            ),
            Self::Invalid(reason) => write!(f, "invalid packet: {reason}"),
        }
    }
}

impl std::error::Error for UmpError {}

/// Which piece of a message a packet is, for messages that may be spread
/// over several packets: system exclusive data, text and names.
///
/// The specification calls this field "status" in data messages and
/// "format" in flex data and stream messages; the four values are the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Part {
    /// The whole message, in this one packet.
    Complete,
    /// The first packet of a longer message.
    Start,
    /// A packet between the first and the last.
    Continue,
    /// The last packet of a longer message.
    End,
}

impl Part {
    pub(crate) const fn from_bits(bits: u32) -> Self {
        match bits & 0x3 {
            0 => Self::Complete,
            1 => Self::Start,
            2 => Self::Continue,
            _ => Self::End,
        }
    }

    pub(crate) const fn bits(self) -> u32 {
        match self {
            Self::Complete => 0,
            Self::Start => 1,
            Self::Continue => 2,
            Self::End => 3,
        }
    }
}

/// One Universal MIDI Packet: one to four 32-bit words.
///
/// A packet is only a container. It knows its message type and therefore
/// its size, and nothing else is checked until [`message`](Self::message)
/// is called. That makes it safe to store, copy and forward packets this
/// crate does not understand, including the reserved message types.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Packet {
    /// Words past `len` are always zero, so derived equality is exact.
    words: [u32; 4],
    len: u8,
}

impl fmt::Debug for Packet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Packet(")?;
        for (index, word) in self.words().iter().enumerate() {
            if index > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{word:08X}")?;
        }
        f.write_str(")")
    }
}

impl Packet {
    /// How many words a packet of the given message type occupies
    /// (Table 4). Only the low four bits of `message_type` are used.
    ///
    /// ```
    /// use manguebyte_midi::ump::Packet;
    ///
    /// assert_eq!(Packet::words_for_type(0x2), 1); // MIDI 1.0 channel voice
    /// assert_eq!(Packet::words_for_type(0x4), 2); // MIDI 2.0 channel voice
    /// assert_eq!(Packet::words_for_type(0xD), 4); // flex data
    /// ```
    pub const fn words_for_type(message_type: u8) -> usize {
        match message_type & 0x0F {
            0x0 | 0x1 | 0x2 | 0x6 | 0x7 => 1,
            0x3 | 0x4 | 0x8 | 0x9 | 0xA => 2,
            0xB | 0xC => 3,
            _ => 4,
        }
    }

    /// Reads the packet at the start of `words`.
    ///
    /// The first word's message type decides the length; words after the
    /// packet are left alone. Fails only if `words` is empty or ends before
    /// the packet does. The contents are not interpreted here.
    pub fn from_words(words: &[u32]) -> Result<Self, UmpError> {
        let needed = match words.first() {
            Some(first) => Self::words_for_type((first >> 28) as u8),
            None => 1,
        };
        if words.len() < needed {
            return Err(UmpError::Truncated {
                needed,
                available: words.len(),
            });
        }
        let mut packet = Self {
            words: [0; 4],
            len: needed as u8,
        };
        packet.words[..needed].copy_from_slice(&words[..needed]);
        Ok(packet)
    }

    /// The words of the packet: one, two, three or four of them.
    #[inline]
    pub fn words(&self) -> &[u32] {
        &self.words[..usize::from(self.len)]
    }

    /// How many words the packet occupies.
    #[inline]
    pub fn word_count(&self) -> usize {
        usize::from(self.len)
    }

    /// Copies the packet to the start of `out` and returns how many words
    /// were written. Fails, writing nothing, if `out` is too short.
    pub fn write(&self, out: &mut [u32]) -> Result<usize, UmpError> {
        let needed = self.word_count();
        match out.get_mut(..needed) {
            Some(target) => {
                target.copy_from_slice(self.words());
                Ok(needed)
            }
            None => Err(UmpError::NoRoom {
                needed,
                available: out.len(),
            }),
        }
    }

    /// The message type: the top four bits of the first word.
    #[inline]
    pub fn message_type(&self) -> u8 {
        (self.words[0] >> 28) as u8
    }

    /// The group the packet is addressed to, `0..=15`.
    ///
    /// `None` for utility and stream messages, which have no group, and
    /// for the reserved message types, whose layout is not defined.
    pub fn group(&self) -> Option<u8> {
        match self.message_type() {
            0x1..=0x5 | 0xD => Some(nibble(self.words[0], 24)),
            _ => None,
        }
    }

    /// A MIDI 1.0 channel voice message as a packet for `group`.
    pub fn from_midi1(group: u8, message: MidiMessage) -> Self {
        Message::Midi1 { group, message }.to_packet()
    }

    /// The MIDI 1.0 channel voice message in the packet, if that is what it
    /// holds. MIDI 2.0 channel voice packets are not converted here; see
    /// [`translate`](crate::translate) for that.
    pub fn to_midi1(&self) -> Option<MidiMessage> {
        match self.message() {
            Ok(Message::Midi1 { message, .. }) => Some(message),
            _ => None,
        }
    }

    /// Interprets the packet.
    ///
    /// Reserved bits are ignored. Returns an error for reserved message
    /// types, for statuses the specification does not define, and for
    /// fields with values that cannot be valid, such as a byte count
    /// larger than the packet.
    pub fn message(&self) -> Result<Message, UmpError> {
        let (w0, w1) = (self.words[0], self.words[1]);
        let message_type = self.message_type();
        let group = nibble(w0, 24);
        match message_type {
            0x0 => decode_utility(w0).map(Message::Utility),
            0x1 => decode_system(w0).map(|message| Message::System { group, message }),
            0x2 => decode_midi1(w0).map(|message| Message::Midi1 { group, message }),
            0x3 => decode_sysex7(w0, w1).map(Message::SysEx7),
            0x4 => decode_voice2(w0, w1).map(|message| Message::Midi2 { group, message }),
            0x5 => decode_data128(&self.words),
            0xD => flex::decode(&self.words).map(Message::Flex),
            0xF => stream::decode(&self.words).map(Message::Stream),
            _ => Err(UmpError::ReservedMessageType(message_type)),
        }
    }

    pub(crate) const fn one(w0: u32) -> Self {
        Self {
            words: [w0, 0, 0, 0],
            len: 1,
        }
    }

    pub(crate) const fn two(w0: u32, w1: u32) -> Self {
        Self {
            words: [w0, w1, 0, 0],
            len: 2,
        }
    }

    pub(crate) const fn four(words: [u32; 4]) -> Self {
        Self { words, len: 4 }
    }

    /// Builds a four-word packet from its sixteen bytes, most significant
    /// first.
    pub(crate) fn from_bytes16(bytes: [u8; 16]) -> Self {
        let mut words = [0u32; 4];
        for (word, chunk) in words.iter_mut().zip(bytes.chunks_exact(4)) {
            *word = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        Self::four(words)
    }
}

impl From<Message> for Packet {
    fn from(message: Message) -> Self {
        message.to_packet()
    }
}

impl TryFrom<Packet> for Message {
    type Error = UmpError;

    fn try_from(packet: Packet) -> Result<Self, UmpError> {
        packet.message()
    }
}

/// The sixteen bytes of a four-word packet, most significant first.
pub(crate) fn bytes16(words: &[u32; 4]) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    for (chunk, word) in bytes.chunks_exact_mut(4).zip(words) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
    bytes
}

#[inline]
pub(crate) const fn nibble(word: u32, shift: u32) -> u8 {
    ((word >> shift) & 0x0F) as u8
}

#[inline]
pub(crate) const fn octet(word: u32, shift: u32) -> u8 {
    ((word >> shift) & 0xFF) as u8
}

/// Seven bits: a field whose eighth bit is reserved.
#[inline]
pub(crate) const fn seven(word: u32, shift: u32) -> u8 {
    ((word >> shift) & 0x7F) as u8
}

#[inline]
pub(crate) const fn header(message_type: u32, group: u8) -> u32 {
    (message_type << 28) | (((group & 0x0F) as u32) << 24)
}

/// Iterates over the packets in a slice of words.
///
/// Each item is a packet, or the error that stopped the iteration: a slice
/// that ends in the middle of a packet yields one
/// [`UmpError::Truncated`] and then nothing more. The words of the
/// unfinished packet stay available through
/// [`remaining`](Packets::remaining), so that a reader which gets its
/// words in pieces can put them in front of the next piece.
///
/// ```
/// use manguebyte_midi::ump::{packets, Message, UmpError, Utility};
///
/// // A timestamp, a MIDI 2.0 packet of two words, and a packet cut short.
/// let stream = [0x0020_1234, 0x40E0_0000, 0x8000_0000, 0x4090_3C00];
/// let mut iter = packets(&stream);
///
/// let timestamp = iter.next().unwrap().unwrap();
/// assert_eq!(timestamp.message(), Ok(Message::Utility(Utility::JrTimestamp(0x1234))));
/// assert_eq!(iter.next().unwrap().unwrap().word_count(), 2);
/// assert_eq!(iter.next(), Some(Err(UmpError::Truncated { needed: 2, available: 1 })));
/// assert_eq!(iter.next(), None);
/// // The first word of the packet that was cut short.
/// assert_eq!(iter.remaining(), &[0x4090_3C00]);
/// ```
pub fn packets(words: &[u32]) -> Packets<'_> {
    Packets {
        rest: words,
        stopped: false,
    }
}

/// The iterator returned by [`packets`].
#[derive(Debug, Clone)]
pub struct Packets<'a> {
    rest: &'a [u32],
    /// Set once a truncation has been reported.
    stopped: bool,
}

impl<'a> Packets<'a> {
    /// The words not yet read. After a truncation error these are the
    /// words of the unfinished packet: at most three.
    pub fn remaining(&self) -> &'a [u32] {
        self.rest
    }
}

impl Iterator for Packets<'_> {
    type Item = Result<Packet, UmpError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.stopped || self.rest.is_empty() {
            return None;
        }
        match Packet::from_words(self.rest) {
            Ok(packet) => {
                self.rest = &self.rest[packet.word_count()..];
                Some(Ok(packet))
            }
            Err(error) => {
                self.stopped = true;
                Some(Err(error))
            }
        }
    }
}

impl core::iter::FusedIterator for Packets<'_> {}

/// What a packet means.
///
/// Fields wider than the format allows are masked to fit when a message is
/// turned into a packet, like [`MidiMessage::to_bytes`] does.
///
/// The specification reserves eight message types for later use, so more
/// variants may be added and a `match` on this type needs a wildcard arm.
/// The same holds for the enums of the kinds of message inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Message {
    /// A utility message: no-op and timing. Addressed to no group.
    Utility(Utility),
    /// A system common or real-time message.
    System {
        /// Group, `0..=15`.
        group: u8,
        /// The message.
        message: SystemMessage,
    },
    /// A MIDI 1.0 channel voice message.
    Midi1 {
        /// Group, `0..=15`.
        group: u8,
        /// The message.
        message: MidiMessage,
    },
    /// Up to six bytes of 7-bit system exclusive data.
    SysEx7(SysEx7),
    /// A MIDI 2.0 channel voice message.
    Midi2 {
        /// Group, `0..=15`.
        group: u8,
        /// The message.
        message: ChannelVoice2,
    },
    /// Up to thirteen bytes of 8-bit system exclusive data.
    SysEx8(SysEx8),
    /// A system exclusive 8 message that its sender gave up on: an end
    /// packet with the byte count `0xF` (section 7.8.1). The data received
    /// so far on this stream must be thrown away.
    SysEx8Aborted {
        /// Group, `0..=15`.
        group: u8,
        /// The stream being abandoned.
        stream_id: u8,
    },
    /// The header of one chunk of a mixed data set.
    MixedDataHeader(MixedDataHeader),
    /// Fourteen bytes of payload of a mixed data set chunk.
    MixedDataPayload(MixedDataPayload),
    /// Flex data: tempo, time and key signatures, chords and text.
    Flex(FlexData),
    /// A UMP stream message: discovery and configuration of an endpoint.
    /// Addressed to no group.
    Stream(StreamMessage),
}

impl Message {
    /// Encodes the message as a packet.
    pub fn to_packet(&self) -> Packet {
        match *self {
            Self::Utility(utility) => Packet::one(match utility {
                Utility::NoOp => 0,
                Utility::JrClock(time) => 0x0010_0000 | u32::from(time),
                Utility::JrTimestamp(time) => 0x0020_0000 | u32::from(time),
                Utility::TicksPerQuarter(ticks) => 0x0030_0000 | u32::from(ticks),
                Utility::DeltaClockstamp(ticks) => 0x0040_0000 | (ticks & 0x000F_FFFF),
            }),
            Self::System { group, message } => {
                let mut bytes = [0u8; 3];
                // Bytes the message does not use stay zero, as section 7.6
                // asks.
                message.write(&mut bytes);
                Packet::one(
                    header(0x1, group)
                        | (u32::from(bytes[0]) << 16)
                        | (u32::from(bytes[1]) << 8)
                        | u32::from(bytes[2]),
                )
            }
            Self::Midi1 { group, message } => {
                let raw = message.to_bytes();
                let mut bytes = [0u8; 3];
                bytes[..raw.as_slice().len()].copy_from_slice(raw.as_slice());
                Packet::one(
                    header(0x2, group)
                        | (u32::from(bytes[0]) << 16)
                        | (u32::from(bytes[1]) << 8)
                        | u32::from(bytes[2]),
                )
            }
            Self::SysEx7(sysex) => sysex.to_packet(),
            Self::Midi2 { group, message } => encode_voice2(group, message),
            Self::SysEx8(sysex) => sysex.to_packet(),
            Self::SysEx8Aborted { group, stream_id } => Packet::four([
                header(0x5, group)
                    | (Part::End.bits() << 20)
                    | (0xF << 16)
                    | (u32::from(stream_id) << 8),
                0,
                0,
                0,
            ]),
            Self::MixedDataHeader(header) => header.to_packet(),
            Self::MixedDataPayload(payload) => payload.to_packet(),
            Self::Flex(flex) => flex::encode(&flex),
            Self::Stream(stream) => stream::encode(&stream),
        }
    }

    /// The group the message is addressed to, or `None` for utility and
    /// stream messages, which belong to the whole endpoint.
    pub fn group(&self) -> Option<u8> {
        match *self {
            Self::Utility(_) | Self::Stream(_) => None,
            Self::System { group, .. }
            | Self::Midi1 { group, .. }
            | Self::Midi2 { group, .. }
            | Self::SysEx8Aborted { group, .. } => Some(group),
            Self::SysEx7(sysex) => Some(sysex.group),
            Self::SysEx8(sysex) => Some(sysex.group),
            Self::MixedDataHeader(header) => Some(header.group),
            Self::MixedDataPayload(payload) => Some(payload.group),
            Self::Flex(flex) => Some(flex.group),
        }
    }
}

/// A utility message (section 7.2).
///
/// Eleven of the sixteen statuses are reserved, so more variants may be
/// added and a `match` on this type needs a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Utility {
    /// Does nothing.
    NoOp,
    /// The sender's clock, in ticks of 1/31250 of a second. It wraps
    /// roughly every 2.1 seconds, and a sender repeats it at least every
    /// 250 ms.
    JrClock(u16),
    /// When the packets that follow are meant to take effect, on the clock
    /// that [`JrClock`](Self::JrClock) announces.
    JrTimestamp(u16),
    /// How many delta clockstamp ticks make a quarter note, `1..=65535`.
    /// Used in MIDI clip files.
    TicksPerQuarter(u16),
    /// Ticks since the previous event in a MIDI clip file. Twenty bits:
    /// `0..=1_048_575`.
    DeltaClockstamp(u32),
}

fn decode_utility(w0: u32) -> Result<Utility, UmpError> {
    let status = nibble(w0, 20);
    Ok(match status {
        0x0 => Utility::NoOp,
        0x1 => Utility::JrClock(w0 as u16),
        0x2 => Utility::JrTimestamp(w0 as u16),
        0x3 => Utility::TicksPerQuarter(w0 as u16),
        0x4 => Utility::DeltaClockstamp(w0 & 0x000F_FFFF),
        _ => {
            return Err(UmpError::UnknownStatus {
                message_type: 0x0,
                status: u16::from(status),
            });
        }
    })
}

fn decode_system(w0: u32) -> Result<SystemMessage, UmpError> {
    let status = octet(w0, 16);
    let (first, second) = (seven(w0, 8), seven(w0, 0));
    Ok(match status {
        0xF1 => SystemMessage::TimeCodeQuarterFrame {
            piece: first >> 4,
            value: first & 0x0F,
        },
        0xF2 => SystemMessage::SongPosition(u16::from(first) | (u16::from(second) << 7)),
        0xF3 => SystemMessage::SongSelect(first),
        0xF6 => SystemMessage::TuneRequest,
        0xF8 => SystemMessage::TimingClock,
        0xFA => SystemMessage::Start,
        0xFB => SystemMessage::Continue,
        0xFC => SystemMessage::Stop,
        0xFE => SystemMessage::ActiveSensing,
        0xFF => SystemMessage::Reset,
        // 0xF0 and 0xF7 are not used in packets: system exclusive data has
        // its own message type (section 7.6).
        _ => {
            return Err(UmpError::UnknownStatus {
                message_type: 0x1,
                status: u16::from(status),
            });
        }
    })
}

fn decode_midi1(w0: u32) -> Result<MidiMessage, UmpError> {
    let status = octet(w0, 16);
    let bytes = [status, seven(w0, 8), seven(w0, 0)];
    MidiMessage::from_bytes(&bytes).ok_or(UmpError::UnknownStatus {
        message_type: 0x2,
        status: u16::from(status >> 4),
    })
}

/// A MIDI 2.0 channel voice message (section 7.4).
///
/// Channels are zero-based (`0..=15`), note numbers, controller numbers of
/// the MIDI 1.0 kind, banks and programs are 7-bit, and values are 16 or 32
/// bits wide with the full range in use: `0` is the minimum and all ones
/// the maximum.
///
/// One opcode is still reserved, so a variant may be added and a `match`
/// on this type needs a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ChannelVoice2 {
    /// A key was released.
    NoteOff {
        /// Channel, `0..=15`.
        channel: u8,
        /// Note number.
        note: u8,
        /// Release velocity.
        velocity: u16,
        /// Extra information about the note.
        attribute: NoteAttribute,
    },
    /// A key was pressed. Unlike in MIDI 1.0, velocity zero is a note-on.
    NoteOn {
        /// Channel, `0..=15`.
        channel: u8,
        /// Note number.
        note: u8,
        /// Attack velocity.
        velocity: u16,
        /// Extra information about the note, such as its exact pitch.
        attribute: NoteAttribute,
    },
    /// Per-note aftertouch.
    PolyPressure {
        /// Channel, `0..=15`.
        channel: u8,
        /// Note number.
        note: u8,
        /// Pressure amount.
        pressure: u32,
    },
    /// A controller of one note whose meaning the MIDI Association
    /// defines.
    RegisteredPerNoteController {
        /// Channel, `0..=15`.
        channel: u8,
        /// Note number.
        note: u8,
        /// Controller number, `0..=255`.
        controller: u8,
        /// Controller value.
        value: u32,
    },
    /// A controller of one note whose meaning is up to the device.
    AssignablePerNoteController {
        /// Channel, `0..=15`.
        channel: u8,
        /// Note number.
        note: u8,
        /// Controller number, `0..=255`.
        controller: u8,
        /// Controller value.
        value: u32,
    },
    /// Tells a receiver how per-note controllers relate to the notes on
    /// one note number (section 7.4.5).
    PerNoteManagement {
        /// Channel, `0..=15`.
        channel: u8,
        /// Note number.
        note: u8,
        /// Notes already playing stop following per-note controllers.
        detach: bool,
        /// Per-note controllers return to their default values.
        reset: bool,
    },
    /// A controller changed.
    ControlChange {
        /// Channel, `0..=15`.
        channel: u8,
        /// Controller number, `0..=127`.
        controller: u8,
        /// Controller value.
        value: u32,
    },
    /// A registered controller was set: the single-message form of a
    /// MIDI 1.0 RPN sequence.
    RegisteredController {
        /// Channel, `0..=15`.
        channel: u8,
        /// Bank, the RPN's most significant byte.
        bank: u8,
        /// Index within the bank, the RPN's least significant byte.
        index: u8,
        /// The value.
        value: u32,
    },
    /// An assignable controller was set: the single-message form of a
    /// MIDI 1.0 NRPN sequence.
    AssignableController {
        /// Channel, `0..=15`.
        channel: u8,
        /// Bank, the NRPN's most significant byte.
        bank: u8,
        /// Index within the bank, the NRPN's least significant byte.
        index: u8,
        /// The value.
        value: u32,
    },
    /// A registered controller was moved up or down from where it was.
    RelativeRegisteredController {
        /// Channel, `0..=15`.
        channel: u8,
        /// Bank.
        bank: u8,
        /// Index within the bank.
        index: u8,
        /// How far to move, in the units of the absolute value.
        delta: i32,
    },
    /// An assignable controller was moved up or down from where it was.
    RelativeAssignableController {
        /// Channel, `0..=15`.
        channel: u8,
        /// Bank.
        bank: u8,
        /// Index within the bank.
        index: u8,
        /// How far to move, in the units of the absolute value.
        delta: i32,
    },
    /// The program changed, optionally selecting a bank first.
    ProgramChange {
        /// Channel, `0..=15`.
        channel: u8,
        /// Program number.
        program: u8,
        /// The bank to select before changing program, or `None` to stay
        /// in the current bank.
        bank: Option<Bank>,
    },
    /// Channel-wide aftertouch.
    ChannelPressure {
        /// Channel, `0..=15`.
        channel: u8,
        /// Pressure amount.
        pressure: u32,
    },
    /// The pitch wheel moved.
    PitchBend {
        /// Channel, `0..=15`.
        channel: u8,
        /// Wheel position, with [`PITCH_BEND_CENTRE`] at rest.
        value: u32,
    },
    /// The pitch of one note was bent.
    PerNotePitchBend {
        /// Channel, `0..=15`.
        channel: u8,
        /// Note number.
        note: u8,
        /// Bend, with [`PITCH_BEND_CENTRE`] meaning none.
        value: u32,
    },
}

/// The value of a MIDI 2.0 pitch bend at rest. Smaller values bend down,
/// larger ones up.
pub const PITCH_BEND_CENTRE: u32 = 0x8000_0000;

impl ChannelVoice2 {
    /// The zero-based channel the message is addressed to.
    pub fn channel(&self) -> u8 {
        match *self {
            Self::NoteOff { channel, .. }
            | Self::NoteOn { channel, .. }
            | Self::PolyPressure { channel, .. }
            | Self::RegisteredPerNoteController { channel, .. }
            | Self::AssignablePerNoteController { channel, .. }
            | Self::PerNoteManagement { channel, .. }
            | Self::ControlChange { channel, .. }
            | Self::RegisteredController { channel, .. }
            | Self::AssignableController { channel, .. }
            | Self::RelativeRegisteredController { channel, .. }
            | Self::RelativeAssignableController { channel, .. }
            | Self::ProgramChange { channel, .. }
            | Self::ChannelPressure { channel, .. }
            | Self::PitchBend { channel, .. }
            | Self::PerNotePitchBend { channel, .. } => channel,
        }
    }
}

/// A bank of programs: the two bank select controllers of MIDI 1.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Bank {
    /// Most significant byte, controller 0 in MIDI 1.0.
    pub msb: u8,
    /// Least significant byte, controller 32 in MIDI 1.0.
    pub lsb: u8,
}

/// Extra information carried by a MIDI 2.0 note-on or note-off
/// (section 7.4.14): a type and sixteen bits of data.
///
/// ```
/// use manguebyte_midi::ump::NoteAttribute;
///
/// // A note a quarter tone above middle C.
/// let attribute = NoteAttribute::pitch(60.5);
/// assert_eq!(attribute.kind(), NoteAttribute::PITCH_7_9);
/// assert_eq!(attribute.data(), (60 << 9) | 256);
/// assert_eq!(attribute.as_pitch(), Some(60.5));
/// assert_eq!(NoteAttribute::NONE.as_pitch(), None);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NoteAttribute {
    kind: u8,
    data: u16,
}

impl NoteAttribute {
    /// Attribute type: no attribute.
    pub const NO_ATTRIBUTE: u8 = 0x00;
    /// Attribute type: data whose meaning the manufacturer defines.
    pub const MANUFACTURER_SPECIFIC: u8 = 0x01;
    /// Attribute type: data whose meaning the active MIDI-CI profile
    /// defines.
    pub const PROFILE_SPECIFIC: u8 = 0x02;
    /// Attribute type: the pitch of the note, as semitones in 7.9
    /// fixed point.
    pub const PITCH_7_9: u8 = 0x03;

    /// No attribute.
    pub const NONE: Self = Self { kind: 0, data: 0 };

    /// An attribute of the given type. Type [`NO_ATTRIBUTE`] carries no
    /// data, so `data` is dropped for it.
    ///
    /// [`NO_ATTRIBUTE`]: Self::NO_ATTRIBUTE
    pub const fn new(kind: u8, data: u16) -> Self {
        Self {
            kind,
            data: if kind == Self::NO_ATTRIBUTE { 0 } else { data },
        }
    }

    /// The exact pitch of the note in semitones, where 69 is A at 440 Hz.
    /// With this attribute the note number only identifies the note; it
    /// no longer says what pitch it has.
    ///
    /// The resolution is 1/512 of a semitone. Values outside `0.0..128.0`
    /// are clamped.
    pub fn pitch(semitones: f32) -> Self {
        let scaled = (semitones * 512.0).round();
        // `as` saturates, and maps a NaN to zero.
        Self::new(Self::PITCH_7_9, scaled.clamp(0.0, 65_535.0) as u16)
    }

    /// The attribute type.
    pub const fn kind(&self) -> u8 {
        self.kind
    }

    /// The attribute data. Zero when there is no attribute.
    pub const fn data(&self) -> u16 {
        self.data
    }

    /// The pitch in semitones, if this is a pitch attribute.
    pub fn as_pitch(&self) -> Option<f32> {
        (self.kind == Self::PITCH_7_9).then(|| f32::from(self.data) / 512.0)
    }
}

impl Default for NoteAttribute {
    fn default() -> Self {
        Self::NONE
    }
}

fn encode_voice2(group: u8, message: ChannelVoice2) -> Packet {
    // (opcode, channel, sixteen bits of index, data word), figure 45.
    let (opcode, channel, index, data): (u32, u8, u32, u32) = match message {
        ChannelVoice2::NoteOff {
            channel,
            note,
            velocity,
            attribute,
        } => (
            0x8,
            channel,
            note_index(note, attribute.kind),
            (u32::from(velocity) << 16) | u32::from(attribute.data),
        ),
        ChannelVoice2::NoteOn {
            channel,
            note,
            velocity,
            attribute,
        } => (
            0x9,
            channel,
            note_index(note, attribute.kind),
            (u32::from(velocity) << 16) | u32::from(attribute.data),
        ),
        ChannelVoice2::PolyPressure {
            channel,
            note,
            pressure,
        } => (0xA, channel, note_index(note, 0), pressure),
        ChannelVoice2::RegisteredPerNoteController {
            channel,
            note,
            controller,
            value,
        } => (0x0, channel, note_index(note, controller), value),
        ChannelVoice2::AssignablePerNoteController {
            channel,
            note,
            controller,
            value,
        } => (0x1, channel, note_index(note, controller), value),
        ChannelVoice2::PerNoteManagement {
            channel,
            note,
            detach,
            reset,
        } => (
            0xF,
            channel,
            note_index(note, (u8::from(detach) << 1) | u8::from(reset)),
            0,
        ),
        ChannelVoice2::ControlChange {
            channel,
            controller,
            value,
        } => (0xB, channel, note_index(controller, 0), value),
        ChannelVoice2::RegisteredController {
            channel,
            bank,
            index,
            value,
        } => (0x2, channel, note_index(bank, index & 0x7F), value),
        ChannelVoice2::AssignableController {
            channel,
            bank,
            index,
            value,
        } => (0x3, channel, note_index(bank, index & 0x7F), value),
        ChannelVoice2::RelativeRegisteredController {
            channel,
            bank,
            index,
            delta,
        } => (0x4, channel, note_index(bank, index & 0x7F), delta as u32),
        ChannelVoice2::RelativeAssignableController {
            channel,
            bank,
            index,
            delta,
        } => (0x5, channel, note_index(bank, index & 0x7F), delta as u32),
        ChannelVoice2::ProgramChange {
            channel,
            program,
            bank,
        } => {
            let program = u32::from(program & 0x7F) << 24;
            match bank {
                // Without a bank the bank fields are zero (section 7.4.9).
                None => (0xC, channel, 0, program),
                Some(bank) => (
                    0xC,
                    channel,
                    1,
                    program | (u32::from(bank.msb & 0x7F) << 8) | u32::from(bank.lsb & 0x7F),
                ),
            }
        }
        ChannelVoice2::ChannelPressure { channel, pressure } => (0xD, channel, 0, pressure),
        ChannelVoice2::PitchBend { channel, value } => (0xE, channel, 0, value),
        ChannelVoice2::PerNotePitchBend {
            channel,
            note,
            value,
        } => (0x6, channel, note_index(note, 0), value),
    };
    Packet::two(
        header(0x4, group) | (opcode << 20) | (u32::from(channel & 0x0F) << 16) | index,
        data,
    )
}

/// The index field of a message about one note: seven bits of note number
/// and eight bits of whatever the message puts beside it.
fn note_index(note: u8, low: u8) -> u32 {
    (u32::from(note & 0x7F) << 8) | u32::from(low)
}

fn decode_voice2(w0: u32, w1: u32) -> Result<ChannelVoice2, UmpError> {
    let opcode = nibble(w0, 20);
    let channel = nibble(w0, 16);
    // The first index byte is a note number, a controller or a bank; all
    // three are seven bits under a reserved bit.
    let (first, second) = (seven(w0, 8), octet(w0, 0));
    let attribute = NoteAttribute::new(second, w1 as u16);
    let velocity = (w1 >> 16) as u16;
    Ok(match opcode {
        0x0 => ChannelVoice2::RegisteredPerNoteController {
            channel,
            note: first,
            controller: second,
            value: w1,
        },
        0x1 => ChannelVoice2::AssignablePerNoteController {
            channel,
            note: first,
            controller: second,
            value: w1,
        },
        0x2 => ChannelVoice2::RegisteredController {
            channel,
            bank: first,
            index: second & 0x7F,
            value: w1,
        },
        0x3 => ChannelVoice2::AssignableController {
            channel,
            bank: first,
            index: second & 0x7F,
            value: w1,
        },
        0x4 => ChannelVoice2::RelativeRegisteredController {
            channel,
            bank: first,
            index: second & 0x7F,
            delta: w1 as i32,
        },
        0x5 => ChannelVoice2::RelativeAssignableController {
            channel,
            bank: first,
            index: second & 0x7F,
            delta: w1 as i32,
        },
        0x6 => ChannelVoice2::PerNotePitchBend {
            channel,
            note: first,
            value: w1,
        },
        0x8 => ChannelVoice2::NoteOff {
            channel,
            note: first,
            velocity,
            attribute,
        },
        0x9 => ChannelVoice2::NoteOn {
            channel,
            note: first,
            velocity,
            attribute,
        },
        0xA => ChannelVoice2::PolyPressure {
            channel,
            note: first,
            pressure: w1,
        },
        0xB => ChannelVoice2::ControlChange {
            channel,
            controller: first,
            value: w1,
        },
        0xC => ChannelVoice2::ProgramChange {
            channel,
            program: seven(w1, 24),
            // Bit 0 of the option flags says whether the bank is meant.
            bank: (second & 0x01 != 0).then(|| Bank {
                msb: seven(w1, 8),
                lsb: seven(w1, 0),
            }),
        },
        0xD => ChannelVoice2::ChannelPressure {
            channel,
            pressure: w1,
        },
        0xE => ChannelVoice2::PitchBend { channel, value: w1 },
        0xF => ChannelVoice2::PerNoteManagement {
            channel,
            note: first,
            detach: second & 0x02 != 0,
            reset: second & 0x01 != 0,
        },
        _ => {
            return Err(UmpError::UnknownStatus {
                message_type: 0x4,
                status: u16::from(opcode),
            });
        }
    })
}

/// One packet of 7-bit system exclusive data (section 7.7): up to six
/// bytes, without the `0xF0` and `0xF7` that frame the message in a
/// MIDI 1.0 byte stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SysEx7 {
    /// Group, `0..=15`.
    pub group: u8,
    /// Which piece of the message this is.
    pub part: Part,
    data: [u8; 6],
    len: u8,
}

impl SysEx7 {
    /// The most data bytes one packet holds.
    pub const MAX_BYTES: usize = 6;

    /// A packet holding `data`. Fails if there are more than six bytes or
    /// if any of them has its high bit set.
    pub fn new(group: u8, part: Part, data: &[u8]) -> Result<Self, UmpError> {
        if data.len() > Self::MAX_BYTES {
            return Err(UmpError::Invalid(
                "more than six bytes in a system exclusive packet",
            ));
        }
        if data.iter().any(|byte| *byte > 0x7F) {
            return Err(UmpError::Invalid("system exclusive data byte above 127"));
        }
        let mut bytes = [0u8; 6];
        bytes[..data.len()].copy_from_slice(data);
        Ok(Self {
            group,
            part,
            data: bytes,
            len: data.len() as u8,
        })
    }

    /// The data bytes, each `0..=127`.
    pub fn data(&self) -> &[u8] {
        &self.data[..usize::from(self.len)]
    }

    fn to_packet(self) -> Packet {
        let [a, b, c, d, e, f] = self.data;
        Packet::two(
            header(0x3, self.group)
                | (self.part.bits() << 20)
                | (u32::from(self.len) << 16)
                | (u32::from(a) << 8)
                | u32::from(b),
            u32::from_be_bytes([c, d, e, f]),
        )
    }
}

fn decode_sysex7(w0: u32, w1: u32) -> Result<SysEx7, UmpError> {
    let status = nibble(w0, 20);
    if status > 0x3 {
        return Err(UmpError::UnknownStatus {
            message_type: 0x3,
            status: u16::from(status),
        });
    }
    let count = usize::from(nibble(w0, 16));
    let [c, d, e, f] = w1.to_be_bytes();
    let bytes = [octet(w0, 8), octet(w0, 0), c, d, e, f];
    // Bytes past the count are padding and are not looked at.
    let data = bytes.get(..count).ok_or(UmpError::Invalid(
        "more than six bytes in a system exclusive packet",
    ))?;
    SysEx7::new(nibble(w0, 24), Part::from_bits(u32::from(status)), data)
}

/// One packet of 8-bit system exclusive data (section 7.8): up to thirteen
/// bytes that may take any value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SysEx8 {
    /// Group, `0..=15`.
    pub group: u8,
    /// Which piece of the message this is.
    pub part: Part,
    /// Tells apart messages that are being sent at the same time. A device
    /// that sends one at a time uses zero.
    pub stream_id: u8,
    data: [u8; 13],
    len: u8,
}

impl SysEx8 {
    /// The most data bytes one packet holds.
    pub const MAX_BYTES: usize = 13;

    /// A packet holding `data`. Fails if there are more than thirteen
    /// bytes.
    pub fn new(group: u8, part: Part, stream_id: u8, data: &[u8]) -> Result<Self, UmpError> {
        if data.len() > Self::MAX_BYTES {
            return Err(UmpError::Invalid(
                "more than thirteen bytes in a system exclusive 8 packet",
            ));
        }
        let mut bytes = [0u8; 13];
        bytes[..data.len()].copy_from_slice(data);
        Ok(Self {
            group,
            part,
            stream_id,
            data: bytes,
            len: data.len() as u8,
        })
    }

    /// The data bytes.
    pub fn data(&self) -> &[u8] {
        &self.data[..usize::from(self.len)]
    }

    fn to_packet(self) -> Packet {
        let mut bytes = [0u8; 16];
        bytes[0] = 0x50 | (self.group & 0x0F);
        // The count includes the stream id (section 7.8).
        bytes[1] = ((self.part.bits() as u8) << 4) | (self.len + 1);
        bytes[2] = self.stream_id;
        bytes[3..].copy_from_slice(&self.data);
        Packet::from_bytes16(bytes)
    }
}

/// The header of one chunk of a mixed data set (section 7.9). A chunk is
/// this packet followed by as many [`MixedDataPayload`] packets as its
/// byte count needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MixedDataHeader {
    /// Group, `0..=15`.
    pub group: u8,
    /// Ties the chunks and payloads of one data set together, `0..=15`.
    pub mds_id: u8,
    /// Size of this chunk in bytes, counting the sixteen of this header.
    pub valid_bytes: u16,
    /// How many chunks the data set has, or zero if not known yet.
    pub chunk_count: u16,
    /// The number of this chunk, counting from one.
    pub chunk_number: u16,
    /// Manufacturer, in the 16-bit form of section 7.10.
    pub manufacturer_id: u16,
    /// Device the data is meant for.
    pub device_id: u16,
    /// First sub-identifier.
    pub sub_id_1: u16,
    /// Second sub-identifier.
    pub sub_id_2: u16,
}

impl MixedDataHeader {
    fn to_packet(self) -> Packet {
        let pair = |high: u16, low: u16| (u32::from(high) << 16) | u32::from(low);
        Packet::four([
            header(0x5, self.group)
                | (0x8 << 20)
                | (u32::from(self.mds_id & 0x0F) << 16)
                | u32::from(self.valid_bytes),
            pair(self.chunk_count, self.chunk_number),
            pair(self.manufacturer_id, self.device_id),
            pair(self.sub_id_1, self.sub_id_2),
        ])
    }
}

/// Fourteen bytes of the payload of a mixed data set chunk (section 7.9).
/// How many of them count is decided by the chunk's header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MixedDataPayload {
    /// Group, `0..=15`.
    pub group: u8,
    /// The data set this belongs to, `0..=15`.
    pub mds_id: u8,
    /// The payload bytes.
    pub data: [u8; 14],
}

impl MixedDataPayload {
    fn to_packet(self) -> Packet {
        let mut bytes = [0u8; 16];
        bytes[0] = 0x50 | (self.group & 0x0F);
        bytes[1] = 0x90 | (self.mds_id & 0x0F);
        bytes[2..].copy_from_slice(&self.data);
        Packet::from_bytes16(bytes)
    }
}

fn decode_data128(words: &[u32; 4]) -> Result<Message, UmpError> {
    let w0 = words[0];
    let group = nibble(w0, 24);
    let status = nibble(w0, 20);
    let bytes = bytes16(words);
    match status {
        0x0..=0x3 => {
            let part = Part::from_bits(u32::from(status));
            let stream_id = bytes[2];
            match nibble(w0, 16) {
                0xF if part == Part::End => Ok(Message::SysEx8Aborted { group, stream_id }),
                // The count includes the stream id, so it is never zero.
                count @ 1..=14 => {
                    SysEx8::new(group, part, stream_id, &bytes[3..2 + usize::from(count)])
                        .map(Message::SysEx8)
                }
                _ => Err(UmpError::Invalid(
                    "byte count of a system exclusive 8 packet out of range",
                )),
            }
        }
        0x8 => Ok(Message::MixedDataHeader(MixedDataHeader {
            group,
            mds_id: nibble(w0, 16),
            valid_bytes: w0 as u16,
            chunk_count: (words[1] >> 16) as u16,
            chunk_number: words[1] as u16,
            manufacturer_id: (words[2] >> 16) as u16,
            device_id: words[2] as u16,
            sub_id_1: (words[3] >> 16) as u16,
            sub_id_2: words[3] as u16,
        })),
        0x9 => {
            let mut data = [0u8; 14];
            data.copy_from_slice(&bytes[2..]);
            Ok(Message::MixedDataPayload(MixedDataPayload {
                group,
                mds_id: nibble(w0, 16),
                data,
            }))
        }
        _ => Err(UmpError::UnknownStatus {
            message_type: 0x5,
            status: u16::from(status),
        }),
    }
}

/// Cuts a payload into pieces of at most `size` bytes and says which part
/// of the whole each piece is. An empty payload gives one empty, complete
/// piece.
#[derive(Debug, Clone)]
pub(crate) struct Pieces<'a> {
    rest: &'a [u8],
    size: usize,
    started: bool,
    done: bool,
}

impl<'a> Pieces<'a> {
    pub(crate) fn new(payload: &'a [u8], size: usize) -> Self {
        Self {
            rest: payload,
            size,
            started: false,
            done: false,
        }
    }
}

impl<'a> Iterator for Pieces<'a> {
    type Item = (Part, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let (piece, rest) = self.rest.split_at(self.rest.len().min(self.size));
        self.rest = rest;
        let last = rest.is_empty();
        let part = match (self.started, last) {
            (false, true) => Part::Complete,
            (false, false) => Part::Start,
            (true, false) => Part::Continue,
            (true, true) => Part::End,
        };
        self.started = true;
        self.done = last;
        Some((part, piece))
    }
}

/// Cuts a system exclusive payload into packets of six bytes.
///
/// `payload` is the message without the `0xF0` that starts it and the
/// `0xF7` that ends it in a MIDI 1.0 byte stream, which is how
/// [`StreamEvent::SysEx`](crate::StreamEvent::SysEx) delivers it. Fails if
/// any byte has its high bit set. Does not allocate.
///
/// ```
/// use manguebyte_midi::ump::sysex7_packets;
///
/// // "GM 2 system on", the example of figure 114.
/// let packets: Vec<_> = sysex7_packets(0, &[0x7E, 0x7F, 0x09, 0x03]).unwrap().collect();
/// assert_eq!(packets.len(), 1);
/// assert_eq!(packets[0].words(), &[0x3004_7E7F, 0x0903_0000]);
/// ```
pub fn sysex7_packets(
    group: u8,
    payload: &[u8],
) -> Result<impl Iterator<Item = Packet> + '_, UmpError> {
    if payload.iter().any(|byte| *byte > 0x7F) {
        return Err(UmpError::Invalid("system exclusive data byte above 127"));
    }
    Ok(
        Pieces::new(payload, SysEx7::MAX_BYTES).map(move |(part, data)| {
            let mut bytes = [0u8; 6];
            bytes[..data.len()].copy_from_slice(data);
            SysEx7 {
                group,
                part,
                data: bytes,
                len: data.len() as u8,
            }
            .to_packet()
        }),
    )
}

/// Cuts a payload of arbitrary bytes into system exclusive 8 packets of
/// thirteen bytes, all carrying `stream_id`. Does not allocate.
pub fn sysex8_packets(
    group: u8,
    stream_id: u8,
    payload: &[u8],
) -> impl Iterator<Item = Packet> + '_ {
    Pieces::new(payload, SysEx8::MAX_BYTES).map(move |(part, data)| {
        let mut bytes = [0u8; 13];
        bytes[..data.len()].copy_from_slice(data);
        SysEx8 {
            group,
            part,
            stream_id,
            data: bytes,
            len: data.len() as u8,
        }
        .to_packet()
    })
}

/// Puts a message that arrives in several packets back together.
///
/// It works for anything split with [`Part`]: 7-bit and 8-bit system
/// exclusive data, flex data text and the names in stream messages. Use
/// one assembler for each source that can be interleaved with others: one
/// per group for 7-bit system exclusive data, one per stream id for the
/// 8-bit kind.
///
/// [`push`](Self::push) is real-time safe. The buffer is allocated when the
/// assembler is created; a message that does not fit is dropped whole
/// rather than delivered cut short.
///
/// ```
/// use manguebyte_midi::ump::{sysex7_packets, Assembler, Message};
///
/// let payload: Vec<u8> = (0..21).collect();
/// let mut assembler = Assembler::new(64);
/// let mut received = Vec::new();
/// for packet in sysex7_packets(0, &payload).unwrap() {
///     if let Ok(Message::SysEx7(piece)) = packet.message() {
///         if let Some(whole) = assembler.push(piece.part, piece.data()) {
///             received = whole.to_vec();
///         }
///     }
/// }
/// assert_eq!(received, payload);
/// ```
// Not `Clone` for the reason `StreamParser` is not: a cloned `Vec` keeps
// only as much capacity as it has items.
#[derive(Debug)]
pub struct Assembler {
    buffer: Vec<u8>,
    capacity: usize,
    receiving: bool,
    overflowed: bool,
}

impl Assembler {
    /// Creates an assembler for messages of up to `capacity` bytes.
    /// Allocates.
    pub fn new(capacity: usize) -> Self {
        Self {
            buffer: Vec::with_capacity(capacity),
            capacity,
            receiving: false,
            overflowed: false,
        }
    }

    /// Takes the data of the next packet and returns the whole message if
    /// this packet completes it.
    ///
    /// A [`Start`](Part::Start) or [`Complete`](Part::Complete) packet
    /// abandons whatever was being received. A
    /// [`Continue`](Part::Continue) or [`End`](Part::End) with no start
    /// before it is ignored.
    pub fn push(&mut self, part: Part, data: &[u8]) -> Option<&[u8]> {
        match part {
            Part::Complete | Part::Start => {
                self.buffer.clear();
                self.overflowed = false;
                self.receiving = part == Part::Start;
                self.append(data);
                (part == Part::Complete && !self.overflowed).then_some(self.buffer.as_slice())
            }
            Part::Continue => {
                if self.receiving {
                    self.append(data);
                }
                None
            }
            Part::End => {
                if !self.receiving {
                    return None;
                }
                self.receiving = false;
                self.append(data);
                (!self.overflowed).then_some(self.buffer.as_slice())
            }
        }
    }

    /// Abandons the message being received. Call it when the sender does,
    /// for instance when another message arrives on the group in the
    /// middle of 7-bit system exclusive data (section 7.7.1), or on
    /// [`Message::SysEx8Aborted`].
    pub fn cancel(&mut self) {
        self.buffer.clear();
        self.receiving = false;
        self.overflowed = false;
    }

    /// Whether a message has started and not yet ended.
    pub fn is_receiving(&self) -> bool {
        self.receiving
    }

    fn append(&mut self, data: &[u8]) {
        if self.overflowed || self.buffer.len() + data.len() > self.capacity {
            self.overflowed = true;
        } else {
            // Within the capacity reserved in `new`, so this cannot
            // allocate.
            self.buffer.extend_from_slice(data);
        }
    }
}

/// The bytes of a zero-padded text field up to the first zero, which marks
/// the end of the text.
pub(crate) fn until_nul(bytes: &[u8]) -> &[u8] {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    &bytes[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(message: Message) {
        let packet = message.to_packet();
        assert_eq!(
            packet.word_count(),
            Packet::words_for_type(packet.message_type()),
            "{message:?}"
        );
        // Through raw words, as a transport would carry it.
        let mut wire = [0xDEAD_BEEFu32; 5];
        let written = packet.write(&mut wire).unwrap();
        assert_eq!(wire[written], 0xDEAD_BEEF);
        let read = Packet::from_words(&wire).unwrap();
        assert_eq!(read, packet);
        assert_eq!(read.message(), Ok(message), "{packet:?}");
        assert_eq!(read.group(), message.group());
        assert_eq!(Packet::from(message), packet);
        assert_eq!(Message::try_from(packet), Ok(message));
    }

    fn voice2(message: ChannelVoice2) -> Message {
        Message::Midi2 { group: 9, message }
    }

    #[test]
    fn packet_sizes_follow_the_message_type_table() {
        // Table 4: sizes in bits for message types 0x0 to 0xF.
        let bits = [
            32, 32, 32, 64, 64, 128, 32, 32, 64, 64, 64, 96, 96, 128, 128, 128,
        ];
        for (message_type, bits) in bits.into_iter().enumerate() {
            assert_eq!(Packet::words_for_type(message_type as u8) * 32, bits);
            let words = [(message_type as u32) << 28, 0, 0, 0, 0];
            let packet = Packet::from_words(&words).unwrap();
            assert_eq!(packet.word_count() * 32, bits);
            assert_eq!(packet.message_type(), message_type as u8);
        }
    }

    #[test]
    fn truncated_input_is_an_error_for_every_message_type() {
        assert_eq!(
            Packet::from_words(&[]),
            Err(UmpError::Truncated {
                needed: 1,
                available: 0
            })
        );
        for message_type in 0..16u32 {
            let words = [message_type << 28; 4];
            let needed = Packet::words_for_type(message_type as u8);
            for available in 0..needed {
                assert_eq!(
                    Packet::from_words(&words[..available]).map(|packet| packet.word_count()),
                    Err(UmpError::Truncated {
                        needed: if available == 0 { 1 } else { needed },
                        available
                    })
                );
            }
            assert!(Packet::from_words(&words[..needed]).is_ok());
        }
    }

    #[test]
    fn writing_needs_room_and_leaves_the_output_alone_without_it() {
        let packet = voice2(ChannelVoice2::PitchBend {
            channel: 0,
            value: 1,
        })
        .to_packet();
        let mut small = [7u32; 1];
        assert_eq!(
            packet.write(&mut small),
            Err(UmpError::NoRoom {
                needed: 2,
                available: 1
            })
        );
        assert_eq!(small, [7]);
    }

    #[test]
    fn the_iterator_walks_packets_of_mixed_sizes_and_stops_at_a_cut() {
        let stream = [
            0x0000_0000, // no-op
            0x2090_3C64, // MIDI 1.0 note on
            0x4090_3C00,
            0xFFFF_0000, // MIDI 2.0 note on
            0xD010_0000,
            0x02FA_F080,
            0,
            0,           // set tempo
            0xB000_0000, // reserved, three words
            1,
            2,
            0x3004_7E7F, // system exclusive, cut after one word
        ];
        let mut iter = packets(&stream);
        let sizes: Vec<usize> = iter
            .by_ref()
            .take(5)
            .map(|packet| packet.unwrap().word_count())
            .collect();
        assert_eq!(sizes, [1, 1, 2, 4, 3]);
        assert_eq!(iter.remaining(), &[0x3004_7E7F]);
        assert_eq!(
            iter.next(),
            Some(Err(UmpError::Truncated {
                needed: 2,
                available: 1
            }))
        );
        // The unfinished packet's word is kept for the caller.
        assert_eq!(iter.remaining(), &[0x3004_7E7F]);
        assert_eq!(iter.next(), None);
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn a_stream_read_in_pieces_gives_the_same_packets_as_read_whole() {
        // Packets of every size, valid and reserved, from random words.
        let mut random = Random(0x5EED_0F5E_ED5E_ED00);
        let mut stream = Vec::new();
        while stream.len() < 600 {
            let first = random.next();
            let size = Packet::words_for_type((first >> 28) as u8);
            stream.push(first);
            stream.extend((1..size).map(|_| random.next()));
        }
        let whole: Vec<Packet> = packets(&stream).map(Result::unwrap).collect();
        assert!(whole.len() > 200);

        // Cut into pieces of fixed sizes and of an irregular pattern. The
        // reader keeps what a piece leaves unfinished and puts it in front
        // of the next piece.
        let patterns: [&[usize]; 7] = [&[1], &[2], &[3], &[4], &[5], &[64], &[1, 7, 2, 300, 3, 1]];
        for pattern in patterns {
            let mut read = Vec::new();
            let mut carried: Vec<u32> = Vec::new();
            let mut position = 0;
            let mut sizes = pattern.iter().copied().cycle();
            while position < stream.len() {
                let end = (position + sizes.next().unwrap()).min(stream.len());
                carried.extend_from_slice(&stream[position..end]);
                position = end;
                let mut iter = packets(&carried);
                for item in iter.by_ref() {
                    match item {
                        Ok(packet) => read.push(packet),
                        Err(error) => assert!(matches!(error, UmpError::Truncated { .. })),
                    }
                }
                let unfinished = iter.remaining().to_vec();
                assert!(unfinished.len() < 4);
                carried = unfinished;
            }
            assert!(carried.is_empty());
            assert_eq!(read, whole, "pieces of {pattern:?}");
        }
    }

    #[test]
    fn utility_messages_round_trip_and_match_the_format_tables() {
        for utility in [
            Utility::NoOp,
            Utility::JrClock(0),
            Utility::JrClock(0xFFFF),
            Utility::JrTimestamp(0x1234),
            Utility::TicksPerQuarter(480),
            Utility::DeltaClockstamp(0),
            Utility::DeltaClockstamp(0x000F_FFFF),
        ] {
            round_trip(Message::Utility(utility));
        }
        // Table 26.
        let words = |utility| Message::Utility(utility).to_packet().words()[0];
        assert_eq!(words(Utility::NoOp), 0x0000_0000);
        assert_eq!(words(Utility::JrClock(0xABCD)), 0x0010_ABCD);
        assert_eq!(words(Utility::JrTimestamp(0xABCD)), 0x0020_ABCD);
        assert_eq!(words(Utility::TicksPerQuarter(0xABCD)), 0x0030_ABCD);
        assert_eq!(words(Utility::DeltaClockstamp(0xF_ABCD)), 0x004F_ABCD);
        // Twenty bits: anything larger is masked.
        assert_eq!(words(Utility::DeltaClockstamp(0xFFF_ABCD)), 0x004F_ABCD);
        // Reserved bits are ignored on the way in.
        assert_eq!(
            Packet::from_words(&[0x0F2F_1234]).unwrap().message(),
            Ok(Message::Utility(Utility::JrTimestamp(0x1234)))
        );
        assert_eq!(
            Packet::from_words(&[0x0050_0000]).unwrap().message(),
            Err(UmpError::UnknownStatus {
                message_type: 0,
                status: 5
            })
        );
    }

    #[test]
    fn system_messages_round_trip_and_match_the_format_table() {
        let all = [
            SystemMessage::TimeCodeQuarterFrame {
                piece: 7,
                value: 15,
            },
            SystemMessage::SongPosition(0),
            SystemMessage::SongPosition(16_383),
            SystemMessage::SongSelect(127),
            SystemMessage::TuneRequest,
            SystemMessage::TimingClock,
            SystemMessage::Start,
            SystemMessage::Continue,
            SystemMessage::Stop,
            SystemMessage::ActiveSensing,
            SystemMessage::Reset,
        ];
        for message in all {
            for group in [0, 15] {
                round_trip(Message::System { group, message });
            }
        }
        // Table 27, with group 5.
        let words = |message| Message::System { group: 5, message }.to_packet().words()[0];
        assert_eq!(
            words(SystemMessage::TimeCodeQuarterFrame { piece: 3, value: 5 }),
            0x15F1_3500
        );
        // Position 0x0A05: least significant seven bits first.
        assert_eq!(words(SystemMessage::SongPosition(0x0A05)), 0x15F2_0514);
        assert_eq!(words(SystemMessage::SongSelect(9)), 0x15F3_0900);
        assert_eq!(words(SystemMessage::TuneRequest), 0x15F6_0000);
        assert_eq!(words(SystemMessage::TimingClock), 0x15F8_0000);
        assert_eq!(words(SystemMessage::Start), 0x15FA_0000);
        assert_eq!(words(SystemMessage::Continue), 0x15FB_0000);
        assert_eq!(words(SystemMessage::Stop), 0x15FC_0000);
        assert_eq!(words(SystemMessage::ActiveSensing), 0x15FE_0000);
        assert_eq!(words(SystemMessage::Reset), 0x15FF_0000);
        // Statuses the table reserves, and ones that are not system at all.
        for status in [0xF0u32, 0xF4, 0xF5, 0xF7, 0xF9, 0xFD, 0x90, 0x00] {
            assert_eq!(
                Packet::from_words(&[0x1000_0000 | (status << 16)])
                    .unwrap()
                    .message(),
                Err(UmpError::UnknownStatus {
                    message_type: 1,
                    status: status as u16
                })
            );
        }
    }

    #[test]
    fn midi1_messages_round_trip_and_copy_their_bytes() {
        let all = [
            MidiMessage::NoteOff {
                channel: 1,
                note: 60,
                velocity: 64,
            },
            MidiMessage::NoteOn {
                channel: 15,
                note: 127,
                velocity: 1,
            },
            MidiMessage::PolyPressure {
                channel: 0,
                note: 12,
                pressure: 99,
            },
            MidiMessage::ControlChange {
                channel: 3,
                controller: 7,
                value: 100,
            },
            MidiMessage::ProgramChange {
                channel: 9,
                program: 42,
            },
            MidiMessage::ChannelPressure {
                channel: 4,
                pressure: 5,
            },
            MidiMessage::PitchBend {
                channel: 2,
                value: -8192,
            },
            MidiMessage::PitchBend {
                channel: 2,
                value: 8191,
            },
        ];
        for message in all {
            round_trip(Message::Midi1 { group: 3, message });
            // Figures 36 and 37: the MIDI 1.0 bytes follow the first byte,
            // and a two-byte message leaves the last byte zero.
            let raw = message.to_bytes();
            let mut bytes = [0u8; 3];
            bytes[..raw.as_slice().len()].copy_from_slice(raw.as_slice());
            let packet = Packet::from_midi1(3, message);
            assert_eq!(
                packet.words()[0].to_be_bytes(),
                [0x23, bytes[0], bytes[1], bytes[2]]
            );
            assert_eq!(packet.to_midi1(), Some(message));
        }
        // A note-on with velocity zero is a note-off, here as elsewhere.
        assert_eq!(
            Packet::from_words(&[0x2090_3C00]).unwrap().to_midi1(),
            Some(MidiMessage::NoteOff {
                channel: 0,
                note: 60,
                velocity: 0
            })
        );
        // Reserved high bits of the data bytes are ignored.
        assert_eq!(
            Packet::from_words(&[0x2090_BCE4]).unwrap().to_midi1(),
            Some(MidiMessage::NoteOn {
                channel: 0,
                note: 60,
                velocity: 100
            })
        );
        for status in [0x00u32, 0x70, 0xF0] {
            let packet = Packet::from_words(&[0x2000_0000 | (status << 16)]).unwrap();
            assert_eq!(packet.to_midi1(), None);
            assert_eq!(
                packet.message(),
                Err(UmpError::UnknownStatus {
                    message_type: 2,
                    status: (status >> 4) as u16
                })
            );
        }
        // Other message types hold no MIDI 1.0 message.
        assert_eq!(Packet::from_words(&[0x10F8_0000]).unwrap().to_midi1(), None);
    }

    fn every_voice2() -> Vec<ChannelVoice2> {
        let attribute = NoteAttribute::new(NoteAttribute::MANUFACTURER_SPECIFIC, 0xBEEF);
        vec![
            ChannelVoice2::NoteOff {
                channel: 15,
                note: 127,
                velocity: 0xFFFF,
                attribute,
            },
            ChannelVoice2::NoteOff {
                channel: 0,
                note: 0,
                velocity: 0,
                attribute: NoteAttribute::NONE,
            },
            ChannelVoice2::NoteOn {
                channel: 1,
                note: 60,
                velocity: 0x8000,
                attribute: NoteAttribute::pitch(60.25),
            },
            // Velocity zero stays a note-on in MIDI 2.0.
            ChannelVoice2::NoteOn {
                channel: 1,
                note: 60,
                velocity: 0,
                attribute: NoteAttribute::new(NoteAttribute::PROFILE_SPECIFIC, 1),
            },
            ChannelVoice2::PolyPressure {
                channel: 2,
                note: 61,
                pressure: 0xFFFF_FFFF,
            },
            ChannelVoice2::RegisteredPerNoteController {
                channel: 3,
                note: 62,
                controller: 255,
                value: 0x1234_5678,
            },
            ChannelVoice2::AssignablePerNoteController {
                channel: 4,
                note: 63,
                controller: 128,
                value: 0x8765_4321,
            },
            ChannelVoice2::PerNoteManagement {
                channel: 5,
                note: 64,
                detach: true,
                reset: false,
            },
            ChannelVoice2::PerNoteManagement {
                channel: 5,
                note: 64,
                detach: false,
                reset: true,
            },
            ChannelVoice2::ControlChange {
                channel: 6,
                controller: 127,
                value: 0x8000_0000,
            },
            ChannelVoice2::RegisteredController {
                channel: 7,
                bank: 127,
                index: 126,
                value: 0xFFFF_FFFF,
            },
            ChannelVoice2::AssignableController {
                channel: 8,
                bank: 1,
                index: 2,
                value: 3,
            },
            ChannelVoice2::RelativeRegisteredController {
                channel: 9,
                bank: 0,
                index: 0,
                delta: i32::MIN,
            },
            ChannelVoice2::RelativeAssignableController {
                channel: 10,
                bank: 4,
                index: 5,
                delta: -1,
            },
            ChannelVoice2::ProgramChange {
                channel: 11,
                program: 127,
                bank: None,
            },
            ChannelVoice2::ProgramChange {
                channel: 11,
                program: 5,
                bank: Some(Bank { msb: 127, lsb: 0 }),
            },
            ChannelVoice2::ChannelPressure {
                channel: 12,
                pressure: 0xDEAD_BEEF,
            },
            ChannelVoice2::PitchBend {
                channel: 13,
                value: PITCH_BEND_CENTRE,
            },
            ChannelVoice2::PerNotePitchBend {
                channel: 14,
                note: 1,
                value: 0,
            },
        ]
    }

    #[test]
    fn midi2_messages_round_trip() {
        for message in every_voice2() {
            round_trip(voice2(message));
            let packet = voice2(message).to_packet();
            assert_eq!(nibble(packet.words()[0], 16), message.channel());
        }
    }

    #[test]
    fn midi2_messages_match_the_format_figures() {
        // Figures 46 to 67, written out field by field with group 9.
        let words = |message| {
            let packet = voice2(message).to_packet();
            [packet.words()[0], packet.words()[1]]
        };
        let all = every_voice2();
        let expected: [[u32; 2]; 19] = [
            [0x498F_7F01, 0xFFFF_BEEF], // note off, attribute type 1
            [0x4980_0000, 0x0000_0000],
            [0x4991_3C03, 0x8000_7880], // note on, pitch 60.25 = 60 << 9 | 128
            [0x4991_3C02, 0x0000_0001],
            [0x49A2_3D00, 0xFFFF_FFFF], // poly pressure
            [0x4903_3EFF, 0x1234_5678], // registered per-note controller
            [0x4914_3F80, 0x8765_4321], // assignable per-note controller
            [0x49F5_4002, 0x0000_0000], // per-note management, D
            [0x49F5_4001, 0x0000_0000], // per-note management, S
            [0x49B6_7F00, 0x8000_0000], // control change
            [0x4927_7F7E, 0xFFFF_FFFF], // registered controller
            [0x4938_0102, 0x0000_0003], // assignable controller
            [0x4949_0000, 0x8000_0000], // relative registered controller
            [0x495A_0405, 0xFFFF_FFFF], // relative assignable controller
            [0x49CB_0000, 0x7F00_0000], // program change, no bank
            [0x49CB_0001, 0x0500_7F00], // program change with bank
            [0x49DC_0000, 0xDEAD_BEEF], // channel pressure
            [0x49ED_0000, 0x8000_0000], // pitch bend
            [0x496E_0100, 0x0000_0000], // per-note pitch bend
        ];
        assert_eq!(all.len(), expected.len());
        for (message, expected) in all.into_iter().zip(expected) {
            assert_eq!(words(message), expected, "{message:?}");
        }
    }

    #[test]
    fn midi2_reading_ignores_reserved_bits_and_rejects_the_reserved_opcode() {
        let read = |w0, w1| Packet::from_words(&[w0, w1]).unwrap().message();
        // Reserved bit above the note number, and reserved low byte.
        assert_eq!(
            read(0x40A0_BCFF, 7),
            Ok(Message::Midi2 {
                group: 0,
                message: ChannelVoice2::PolyPressure {
                    channel: 0,
                    note: 60,
                    pressure: 7
                }
            })
        );
        // A program change without the bank-valid bit ignores the bank
        // bytes; reserved option flags are ignored too.
        assert_eq!(
            read(0x40C0_FFFE, 0x8500_7F7F),
            Ok(Message::Midi2 {
                group: 0,
                message: ChannelVoice2::ProgramChange {
                    channel: 0,
                    program: 5,
                    bank: None
                }
            })
        );
        // Attribute data means nothing without an attribute type.
        assert_eq!(
            read(0x4090_3C00, 0x1234_5678),
            Ok(Message::Midi2 {
                group: 0,
                message: ChannelVoice2::NoteOn {
                    channel: 0,
                    note: 60,
                    velocity: 0x1234,
                    attribute: NoteAttribute::NONE
                }
            })
        );
        assert_eq!(
            read(0x4070_0000, 0),
            Err(UmpError::UnknownStatus {
                message_type: 4,
                status: 7
            })
        );
    }

    #[test]
    fn fields_too_wide_for_the_format_are_masked_when_writing() {
        let packet = Message::Midi2 {
            group: 0x1F,
            message: ChannelVoice2::RegisteredController {
                channel: 0x2F,
                bank: 0xFF,
                index: 0xFF,
                value: 1,
            },
        }
        .to_packet();
        assert_eq!(packet.words(), &[0x4F2F_7F7F, 1]);
        assert_eq!(
            Packet::from_midi1(
                0xF3,
                MidiMessage::NoteOn {
                    channel: 0x12,
                    note: 200,
                    velocity: 255
                }
            )
            .words(),
            &[0x2392_487F]
        );
    }

    #[test]
    fn note_attributes_keep_their_invariants() {
        assert_eq!(NoteAttribute::new(0, 99), NoteAttribute::NONE);
        assert_eq!(NoteAttribute::default(), NoteAttribute::NONE);
        assert_eq!(NoteAttribute::pitch(-3.0).data(), 0);
        assert_eq!(NoteAttribute::pitch(500.0).data(), 0xFFFF);
        assert_eq!(NoteAttribute::pitch(f32::NAN).data(), 0);
        // 7.9 fixed point: one step is 1/512 of a semitone.
        assert_eq!(NoteAttribute::pitch(69.0).data(), 69 << 9);
        assert_eq!(NoteAttribute::pitch(1.0 / 512.0).data(), 1);
        for raw in [0u16, 1, 511, 512, 0x8A00, 0xFFFF] {
            let attribute = NoteAttribute::new(NoteAttribute::PITCH_7_9, raw);
            let pitch = attribute.as_pitch().unwrap();
            assert_eq!(NoteAttribute::pitch(pitch), attribute);
        }
        assert_eq!(
            NoteAttribute::new(NoteAttribute::MANUFACTURER_SPECIFIC, 5).as_pitch(),
            None
        );
    }

    #[test]
    fn sysex7_packets_follow_the_examples_of_the_specification() {
        // Figure 112: five bytes fit one complete packet, padded with one
        // zero byte.
        let five: Vec<Packet> = sysex7_packets(2, &[1, 2, 3, 4, 5]).unwrap().collect();
        assert_eq!(five.len(), 1);
        assert_eq!(five[0].words(), &[0x3205_0102, 0x0304_0500]);

        // Figure 113: twenty-one bytes are a start with six, two continues
        // with six and an end with three.
        let payload: Vec<u8> = (1..=21).collect();
        let packets: Vec<Packet> = sysex7_packets(0, &payload).unwrap().collect();
        let headers: Vec<u32> = packets.iter().map(|p| p.words()[0] >> 16).collect();
        assert_eq!(headers, [0x3016, 0x3026, 0x3026, 0x3033]);
        assert_eq!(packets[3].words(), &[0x3033_1314, 0x1500_0000]);

        // Figure 114: GM 2 system on, to all devices.
        let gm2: Vec<Packet> = sysex7_packets(0, &[0x7E, 0x7F, 0x09, 0x03])
            .unwrap()
            .collect();
        assert_eq!(gm2[0].words(), &[0x3004_7E7F, 0x0903_0000]);

        // An empty message is one complete packet with no bytes, and an
        // exact multiple of six ends with a full packet.
        let empty: Vec<Packet> = sysex7_packets(0, &[]).unwrap().collect();
        assert_eq!(empty[0].words(), &[0x3000_0000, 0]);
        let twelve: Vec<Packet> = sysex7_packets(0, &[9; 12]).unwrap().collect();
        assert_eq!(twelve.len(), 2);
        assert_eq!(twelve[1].words()[0] >> 16, 0x3036);

        assert!(sysex7_packets(0, &[1, 0x80]).is_err());
    }

    #[test]
    fn sysex7_round_trips_and_rejects_what_cannot_be_valid() {
        for part in [Part::Complete, Part::Start, Part::Continue, Part::End] {
            for len in 0..=6 {
                let data = [0x7F, 1, 2, 3, 4, 0x55];
                let sysex = SysEx7::new(11, part, &data[..len]).unwrap();
                assert_eq!(sysex.data(), &data[..len]);
                round_trip(Message::SysEx7(sysex));
            }
        }
        assert!(SysEx7::new(0, Part::Complete, &[0; 7]).is_err());
        assert!(SysEx7::new(0, Part::Complete, &[0x80]).is_err());

        let read = |w0, w1| Packet::from_words(&[w0, w1]).unwrap().message();
        // A count of seven or more cannot fit.
        for count in 7..16u32 {
            assert!(matches!(
                read(0x3000_0000 | (count << 16), 0),
                Err(UmpError::Invalid(_))
            ));
        }
        // A data byte with its high bit set is corrupt, but only inside
        // the count: padding is not looked at.
        assert!(matches!(read(0x3002_0180, 0), Err(UmpError::Invalid(_))));
        assert!(read(0x3001_01FF, 0xFFFF_FFFF).is_ok());
        assert_eq!(
            read(0x3040_0000, 0),
            Err(UmpError::UnknownStatus {
                message_type: 3,
                status: 4
            })
        );
    }

    #[test]
    fn sysex8_and_mixed_data_round_trip_and_match_the_tables() {
        let data: Vec<u8> = (0xF0..=0xFC).collect();
        for part in [Part::Complete, Part::Start, Part::Continue, Part::End] {
            for len in 0..=13 {
                let sysex = SysEx8::new(6, part, 0xAB, &data[..len]).unwrap();
                assert_eq!(sysex.data(), &data[..len]);
                round_trip(Message::SysEx8(sysex));
            }
        }
        assert!(SysEx8::new(0, Part::Complete, 0, &[0; 14]).is_err());
        round_trip(Message::SysEx8Aborted {
            group: 6,
            stream_id: 0xAB,
        });

        // Table 25: the count includes the stream id.
        let three = SysEx8::new(6, Part::Start, 0xAB, &[0xF0, 0xF1]).unwrap();
        assert_eq!(
            Message::SysEx8(three).to_packet().words(),
            &[0x5613_ABF0, 0xF100_0000, 0, 0]
        );
        let full = SysEx8::new(0, Part::Complete, 1, &data).unwrap();
        assert_eq!(
            Message::SysEx8(full).to_packet().words(),
            &[0x500E_01F0, 0xF1F2_F3F4, 0xF5F6_F7F8, 0xF9FA_FBFC]
        );
        assert_eq!(
            Message::SysEx8Aborted {
                group: 6,
                stream_id: 0xAB
            }
            .to_packet()
            .words(),
            &[0x563F_AB00, 0, 0, 0]
        );

        let read = |w0: u32| Packet::from_words(&[w0, 0, 0, 0]).unwrap().message();
        // A count of zero leaves no room for the stream id; fifteen is the
        // abort marker and only an end packet may carry it.
        assert!(matches!(read(0x5000_0000), Err(UmpError::Invalid(_))));
        assert!(matches!(read(0x500F_0000), Err(UmpError::Invalid(_))));
        assert!(matches!(read(0x502F_0000), Err(UmpError::Invalid(_))));
        for status in [4u32, 5, 6, 7, 10, 15] {
            assert_eq!(
                read(0x5000_0000 | (status << 20)),
                Err(UmpError::UnknownStatus {
                    message_type: 5,
                    status: status as u16
                })
            );
        }

        let header = MixedDataHeader {
            group: 2,
            mds_id: 0xC,
            valid_bytes: 0x0102,
            chunk_count: 0x0304,
            chunk_number: 0x0506,
            manufacturer_id: 0x0708,
            device_id: 0x090A,
            sub_id_1: 0x0B0C,
            sub_id_2: 0x0D0E,
        };
        round_trip(Message::MixedDataHeader(header));
        // Figure 86.
        assert_eq!(
            Message::MixedDataHeader(header).to_packet().words(),
            &[0x528C_0102, 0x0304_0506, 0x0708_090A, 0x0B0C_0D0E]
        );
        let payload = MixedDataPayload {
            group: 2,
            mds_id: 0xC,
            data: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 0xFF],
        };
        round_trip(Message::MixedDataPayload(payload));
        assert_eq!(
            Message::MixedDataPayload(payload).to_packet().words(),
            &[0x529C_0102, 0x0304_0506, 0x0708_090A, 0x0B0C_0DFF]
        );
    }

    #[test]
    fn sysex8_packets_cut_a_payload_into_thirteens() {
        let payload: Vec<u8> = (0..30).map(|i| 0xE0 + i).collect();
        let mut assembler = Assembler::new(64);
        let mut parts = Vec::new();
        let mut whole = Vec::new();
        for packet in sysex8_packets(4, 7, &payload) {
            let Ok(Message::SysEx8(piece)) = packet.message() else {
                panic!("not a system exclusive 8 packet: {packet:?}");
            };
            assert_eq!((piece.group, piece.stream_id), (4, 7));
            parts.push((piece.part, piece.data().len()));
            if let Some(message) = assembler.push(piece.part, piece.data()) {
                whole = message.to_vec();
            }
        }
        assert_eq!(
            parts,
            [(Part::Start, 13), (Part::Continue, 13), (Part::End, 4)]
        );
        assert_eq!(whole, payload);
    }

    #[test]
    fn the_assembler_drops_what_does_not_fit_and_what_has_no_start() {
        let mut assembler = Assembler::new(8);
        // No start: ignored.
        assert_eq!(assembler.push(Part::Continue, &[1]), None);
        assert_eq!(assembler.push(Part::End, &[2]), None);
        // Too long: dropped whole, and the next message is unaffected.
        assert_eq!(assembler.push(Part::Start, &[0; 6]), None);
        assert!(assembler.is_receiving());
        assert_eq!(assembler.push(Part::Continue, &[0; 6]), None);
        assert_eq!(assembler.push(Part::End, &[0; 1]), None);
        assert!(!assembler.is_receiving());
        assert_eq!(assembler.push(Part::Complete, &[0; 9]), None);
        assert_eq!(assembler.push(Part::Complete, &[5; 8]), Some(&[5u8; 8][..]));
        // A new start abandons the message in progress.
        assert_eq!(assembler.push(Part::Start, &[1, 2]), None);
        assert_eq!(assembler.push(Part::Start, &[3]), None);
        assert_eq!(assembler.push(Part::End, &[4]), Some(&[3u8, 4][..]));
        // So does cancelling.
        assert_eq!(assembler.push(Part::Start, &[1, 2]), None);
        assembler.cancel();
        assert_eq!(assembler.push(Part::End, &[4]), None);
        // An empty message is still a message.
        assert_eq!(assembler.push(Part::Complete, &[]), Some(&[][..]));
    }

    #[test]
    fn reserved_message_types_are_kept_but_not_interpreted() {
        for message_type in [0x6u8, 0x7, 0x8, 0x9, 0xA, 0xB, 0xC, 0xE] {
            let words = [(u32::from(message_type) << 28) | 0x0123_4567, 1, 2, 3];
            let packet = Packet::from_words(&words).unwrap();
            assert_eq!(
                packet.message(),
                Err(UmpError::ReservedMessageType(message_type))
            );
            assert_eq!(packet.group(), None);
            assert_eq!(packet.words(), &words[..packet.word_count()]);
        }
    }

    /// xorshift64*, enough to throw varied words at the parser.
    struct Random(u64);

    impl Random {
        fn next(&mut self) -> u32 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
        }
    }

    #[test]
    fn random_words_never_panic_and_reading_is_stable() {
        let mut random = Random(0x9E37_79B9_7F4A_7C15);
        let mut decoded = 0usize;
        let mut rejected = 0usize;
        for round in 0..100_000 {
            let mut words = [0u32; 9];
            for word in &mut words {
                *word = random.next();
            }
            // Random bits rarely land on the narrow status ranges, so half
            // of the rounds force a small status field.
            if round % 2 == 0 {
                words[0] &= 0xF0FF_FFFF | ((random.next() & 0x0F) << 24);
                words[0] &= 0xFF0F_FFFF | ((random.next() & 0x03) << 20);
            }
            let len = (random.next() % 10) as usize;
            for item in packets(&words[..len.min(9)]) {
                let Ok(packet) = item else { continue };
                assert_eq!(
                    packet.word_count(),
                    Packet::words_for_type(packet.message_type())
                );
                match packet.message() {
                    Ok(message) => {
                        decoded += 1;
                        // Writing what was read and reading it again gives
                        // the same message: nothing is lost or invented.
                        let again = message.to_packet();
                        assert_eq!(again.message(), Ok(message), "{packet:?}");
                        assert_eq!(again.message_type(), packet.message_type());
                        assert_eq!(again.message().unwrap().to_packet(), again);
                    }
                    Err(error) => {
                        rejected += 1;
                        assert!(!matches!(
                            error,
                            UmpError::Truncated { .. } | UmpError::NoRoom { .. }
                        ));
                        // Errors can be shown.
                        assert!(!error.to_string().is_empty());
                    }
                }
            }
        }
        // Both paths were exercised, not just one. With this seed, 27 843
        // packets are read as messages and 152 890 are rejected.
        assert!(decoded > 20_000, "decoded {decoded}");
        assert!(rejected > 100_000, "rejected {rejected}");
    }

    /// Reads a packet and, if it is a message, counts it under its message
    /// type and checks that writing and reading it again changes nothing.
    fn tally(accepted: &mut [usize; 16], words: [u32; 4]) {
        let packet = Packet::from_words(&words).unwrap();
        if let Ok(message) = packet.message() {
            accepted[usize::from(packet.message_type())] += 1;
            assert_eq!(message.to_packet().message(), Ok(message), "{packet:?}");
        }
    }

    #[test]
    fn exactly_the_defined_statuses_are_accepted() {
        // Every value of every status field, over random data. What is
        // accepted must be the set the specification defines, no more and
        // no less.
        let mut random = Random(0xC0FF_EE00_DEAD_BEEF);
        // Data with every byte below 0x80, so that 7-bit system exclusive
        // bytes are valid whenever their header is.
        let mut data = move || random.next() & 0x7F7F_7F7F;
        let mut accepted = [0usize; 16];

        // Utility: four bits of status.
        for status in 0..16u32 {
            tally(
                &mut accepted,
                [(status << 20) | (data() & 0xF_FFFF), 0, 0, 0],
            );
        }
        // System: a status byte.
        for status in 0..256u32 {
            let word = 0x1000_0000 | (status << 16) | (data() & 0xFFFF);
            tally(&mut accepted, [word, 0, 0, 0]);
        }
        // MIDI 1.0 and MIDI 2.0 channel voice: four bits of opcode.
        for opcode in 0..16u32 {
            let rest = (opcode << 20) | (data() & 0xF_FFFF);
            tally(&mut accepted, [0x2000_0000 | rest, 0, 0, 0]);
            tally(&mut accepted, [0x4000_0000 | rest, data(), 0, 0]);
        }
        // Data messages: four bits of status and four of byte count or id.
        for status_and_count in 0..256u32 {
            let rest = (status_and_count << 16) | (data() & 0xFFFF);
            tally(&mut accepted, [0x3000_0000 | rest, data(), 0, 0]);
            tally(&mut accepted, [0x5000_0000 | rest, data(), data(), data()]);
        }
        // Flex data: form and address, then status bank and status.
        for form_and_address in 0..16u32 {
            for bank_and_status in 0..=0xFFFFu32 {
                let word = 0xD000_0000 | (form_and_address << 20) | bank_and_status;
                tally(&mut accepted, [word, data(), data(), data()]);
            }
        }
        // Stream: two bits of form and ten of status.
        for form_and_status in 0..4096u32 {
            let word = 0xF000_0000 | (form_and_status << 16) | (data() & 0xFFFF);
            tally(&mut accepted, [word, data(), data(), data()]);
        }
        // Reserved message types, whatever they hold.
        for message_type in [0x6u32, 0x7, 0x8, 0x9, 0xA, 0xB, 0xC, 0xE] {
            for _ in 0..256 {
                let word = (message_type << 28) | (data() & 0x0FFF_FFFF);
                tally(&mut accepted, [word, data(), data(), data()]);
            }
        }

        let expected = [
            // Utility: no-op, two timestamps, two delta clockstamps.
            5,
            // System: four common and six real-time messages.
            10,
            // MIDI 1.0 channel voice: seven messages.
            7,
            // System exclusive: four statuses with zero to six bytes.
            4 * 7,
            // MIDI 2.0 channel voice: sixteen opcodes less the reserved one.
            15,
            // System exclusive 8: four statuses with one to fourteen bytes,
            // and the abort. Mixed data: header and payload for sixteen ids.
            4 * 14 + 1 + 2 * 16,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            // Flex data, each for two kinds of address: five setup messages
            // in one form, thirteen metadata and five performance texts in
            // four forms.
            2 * (5 + 4 * 13 + 4 * 5),
            0,
            // Stream: nine messages of fixed size and three texts in four
            // forms.
            9 + 4 * 3,
        ];
        assert_eq!(accepted, expected);
    }

    #[test]
    fn every_status_of_every_message_type_is_read_without_panicking() {
        // All sixteen message types with all 4096 values of the twelve
        // bits that hold status fields, over all-ones and all-zero data.
        for fill in [0u32, 0xFFFF_FFFF, 0x7F7F_7F7F, 0x0101_0101] {
            for top in 0..=0xFFFFu32 {
                let words = [(top << 16) | (fill & 0xFFFF), fill, fill, fill];
                let packet = Packet::from_words(&words).unwrap();
                if let Ok(message) = packet.message() {
                    assert_eq!(message.to_packet().message(), Ok(message));
                }
            }
        }
    }

    #[test]
    fn errors_describe_themselves() {
        let text = |error: UmpError| error.to_string();
        assert_eq!(
            text(UmpError::Truncated {
                needed: 2,
                available: 1
            }),
            "packet needs 2 words but only 1 are left"
        );
        assert_eq!(
            text(UmpError::ReservedMessageType(0xE)),
            "message type 0xE is reserved"
        );
        assert_eq!(
            text(UmpError::UnknownStatus {
                message_type: 4,
                status: 7
            }),
            "status 0x7 is not defined for message type 0x4"
        );
        assert_eq!(text(UmpError::Invalid("x")), "invalid packet: x");
        assert!(
            text(UmpError::NoRoom {
                needed: 4,
                available: 0
            })
            .contains("4 words")
        );
    }
}
