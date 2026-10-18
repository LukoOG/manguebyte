//! Translation between the MIDI 1.0 and MIDI 2.0 protocols.
//!
//! The two protocols carry the same music at different resolutions and
//! group some things differently: what MIDI 1.0 says with a bank select
//! pair and a program change, or with a four-message parameter sequence,
//! MIDI 2.0 says in one message. This module implements the "default
//! translation" that the specification requires every translator to offer:
//!
//! - [`scale_up`] and [`scale_down`], with shorthands for the widths that
//!   occur, convert values between 7, 14, 16 and 32 bits.
//! - [`Midi1ToMidi2`] turns a stream of MIDI 1.0 messages into MIDI 2.0
//!   messages. It keeps state, because several MIDI 1.0 messages can
//!   become one MIDI 2.0 message.
//! - [`midi2_to_midi1`] turns one MIDI 2.0 message into the MIDI 1.0
//!   messages that stand for it.
//! - [`UmpTranslator`] and [`packet_to_midi1`] do the same for
//!   [`Packet`]s.
//! - [`Midi2ToMpe`] is the alternate translation for what shapes one
//!   note: it writes MIDI 2.0 out as MPE, with a member channel for each
//!   note.
//!
//! Written from appendix D of "Universal MIDI Packet (UMP) Format and
//! MIDI 2.0 Protocol", version 1.1.1 (M2-104-UM). Section numbers in
//! comments refer to that document. What "reset all controllers" does to
//! the selected parameter number is taken from recommended practice
//! RP-015.
//!
//! Everything here is real-time safe.
//!
//! ```
//! use manguebyte_midi::translate::{midi2_to_midi1, Midi1ToMidi2};
//! use manguebyte_midi::ump::ChannelVoice2;
//! use manguebyte_midi::MidiMessage;
//!
//! let cc = |controller, value| MidiMessage::ControlChange { channel: 0, controller, value };
//! let mut translator = Midi1ToMidi2::new();
//!
//! // Bank select and program change become one message.
//! assert!(translator.translate(cc(0, 1)).is_empty());
//! assert!(translator.translate(cc(32, 2)).is_empty());
//! let translated = translator.translate(MidiMessage::ProgramChange { channel: 0, program: 3 });
//! let [program] = translated.as_slice() else { panic!("expected one message") };
//! assert!(matches!(program, ChannelVoice2::ProgramChange { bank: Some(_), .. }));
//!
//! // And three again on the way back.
//! let back = midi2_to_midi1(*program);
//! assert_eq!(
//!     back.as_slice(),
//!     [cc(0, 1), cc(32, 2), MidiMessage::ProgramChange { channel: 0, program: 3 }]
//! );
//! ```
//!
//! # Limits
//!
//! Only the default translation is implemented. Per-note controllers,
//! per-note pitch bend, per-note management and relative controllers have
//! no MIDI 1.0 form and translate to nothing (section D.2.8); mapping them
//! to MPE is an alternate translation, which [`Midi2ToMpe`] does.
//! High-resolution velocity (controller 88) is passed through as an
//! ordinary controller in both directions.
//!
//! MIDI 2.0 sets a parameter with one message that carries the whole
//! value. A MIDI 1.0 fine data byte (controller 38) can therefore only be
//! translated when the translator knows the coarse byte that goes with
//! it. If it has not seen one since the parameter was selected, or an
//! increment or decrement has moved the value since, the fine byte is
//! dropped. See [`Midi1ToMidi2`].

use crate::ump::{Bank, ChannelVoice2, Message, NoteAttribute, Packet};
use crate::{MessageList, MidiMessage, SystemMessage};

mod to_mpe;

pub use to_mpe::Midi2ToMpe;

/// All ones in the low `bits` bits.
const fn mask(bits: u32) -> u32 {
    if bits >= 32 {
        u32::MAX
    } else {
        (1 << bits) - 1
    }
}

/// Widens a value from `from_bits` to `to_bits` so that the whole range
/// stays in use: the minimum stays the minimum, the centre stays the
/// centre and the maximum becomes the maximum.
///
/// This is the "min-centre-max" method of section D.1.3. Values up to the
/// centre are shifted left. Above the centre, the bits below the top bit
/// are also repeated into the new low bits, which spreads the upper half
/// evenly up to the maximum.
///
/// Both widths are clamped to `1..=32` and `value` is masked to
/// `from_bits`. If `to_bits` is not larger than `from_bits` the result is
/// that of [`scale_down`]. The maximum only maps to the maximum when
/// `from_bits` is at least 2.
///
/// ```
/// use manguebyte_midi::translate::scale_up;
///
/// // The examples of the specification, 7 to 16 bits.
/// assert_eq!(scale_up(10, 7, 16), 0x1400);
/// assert_eq!(scale_up(64, 7, 16), 0x8000);
/// assert_eq!(scale_up(87, 7, 16), 0xAEBA);
/// assert_eq!(scale_up(127, 7, 16), 0xFFFF);
/// ```
pub const fn scale_up(value: u32, from_bits: u32, to_bits: u32) -> u32 {
    let from_bits = clamp_bits(from_bits);
    let to_bits = clamp_bits(to_bits);
    let value = value & mask(from_bits);
    if to_bits <= from_bits {
        return value >> (from_bits - to_bits);
    }
    let shift = to_bits - from_bits;
    let shifted = value << shift;
    let centre = 1u32 << (from_bits - 1);
    // One bit has no "below the top bit" to repeat.
    if value <= centre || from_bits < 2 {
        return shifted;
    }
    // The bits to repeat, placed directly under the shifted value and then
    // again and again further down until they fall off the end.
    let repeat_bits = from_bits - 1;
    let mut repeat = value & (centre - 1);
    if shift > repeat_bits {
        repeat <<= shift - repeat_bits;
    } else {
        repeat >>= repeat_bits - shift;
    }
    let mut result = shifted;
    while repeat != 0 {
        result |= repeat;
        repeat >>= repeat_bits;
    }
    result
}

/// Narrows a value from `from_bits` to `to_bits` by dropping its low bits
/// (section D.1.4). This undoes [`scale_up`] exactly.
///
/// Both widths are clamped to `1..=32` and `value` is masked to
/// `from_bits`. If `to_bits` is larger than `from_bits` the result is that
/// of [`scale_up`].
pub const fn scale_down(value: u32, from_bits: u32, to_bits: u32) -> u32 {
    let from_bits = clamp_bits(from_bits);
    let to_bits = clamp_bits(to_bits);
    if to_bits > from_bits {
        scale_up(value, from_bits, to_bits)
    } else {
        (value & mask(from_bits)) >> (from_bits - to_bits)
    }
}

const fn clamp_bits(bits: u32) -> u32 {
    if bits < 1 {
        1
    } else if bits > 32 {
        32
    } else {
        bits
    }
}

/// A 7-bit value as 16 bits: MIDI 1.0 velocity to MIDI 2.0 velocity.
#[inline]
pub const fn scale_7_to_16(value: u8) -> u16 {
    scale_up(value as u32, 7, 16) as u16
}

/// A 7-bit value as 32 bits: MIDI 1.0 controllers and pressure to
/// MIDI 2.0.
#[inline]
pub const fn scale_7_to_32(value: u8) -> u32 {
    scale_up(value as u32, 7, 32)
}

/// A 14-bit value as 32 bits: MIDI 1.0 pitch bend and parameter values to
/// MIDI 2.0.
#[inline]
pub const fn scale_14_to_32(value: u16) -> u32 {
    scale_up(value as u32, 14, 32)
}

/// A 16-bit value as 7 bits.
#[inline]
pub const fn scale_16_to_7(value: u16) -> u8 {
    (value >> 9) as u8
}

/// A 32-bit value as 7 bits.
#[inline]
pub const fn scale_32_to_7(value: u32) -> u8 {
    (value >> 25) as u8
}

/// A 32-bit value as 14 bits.
#[inline]
pub const fn scale_32_to_14(value: u32) -> u16 {
    (value >> 18) as u16
}

/// What the translator remembers about one channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChannelState {
    /// The bank select controllers as last received, if either was.
    bank: Option<Bank>,
    /// Which pair of parameter number controllers was touched last.
    registered: Option<bool>,
    /// Controllers 101 and 100, each once it has been received.
    rpn: [Option<u8>; 2],
    /// Controllers 99 and 98, each once it has been received.
    nrpn: [Option<u8>; 2],
    /// Controller 6 as received since the current parameter was selected,
    /// while it still is the coarse half of the parameter's value.
    data_msb: Option<u8>,
}

impl ChannelState {
    const NEW: Self = Self {
        bank: None,
        registered: None,
        rpn: [None; 2],
        nrpn: [None; 2],
        data_msb: None,
    };

    /// The MIDI 2.0 message for the current parameter with a 14-bit value,
    /// if both halves of a parameter number are known and it is not the
    /// null parameter.
    fn controller(&self, channel: u8, value: u16) -> Option<ChannelVoice2> {
        let registered = self.registered?;
        let [bank, index] = if registered { self.rpn } else { self.nrpn };
        let (bank, index) = (bank?, index?);
        if bank == 0x7F && index == 0x7F {
            // The null function is not translated (section D.3.3).
            return None;
        }
        let value = scale_14_to_32(value);
        Some(if registered {
            ChannelVoice2::RegisteredController {
                channel,
                bank,
                index,
                value,
            }
        } else {
            ChannelVoice2::AssignableController {
                channel,
                bank,
                index,
                value,
            }
        })
    }

    /// The message for the coarse data byte alone, with a fine byte of
    /// zero, if there is a coarse byte and a parameter for it.
    fn coarse_alone(&self, channel: u8) -> Option<ChannelVoice2> {
        self.controller(channel, u16::from(self.data_msb?) << 7)
    }

    /// Takes one of the four parameter number controllers, 98 to 101.
    fn select(&mut self, number: u8, value: u8) {
        let before = (self.registered, self.rpn, self.nrpn);
        let registered = number >= 100;
        let pair = if registered {
            &mut self.rpn
        } else {
            &mut self.nrpn
        };
        // 101 and 99 are the most significant halves.
        pair[usize::from(number % 2 == 0)] = Some(value);
        self.registered = Some(registered);
        // A coarse byte belongs to the parameter it was sent for. Naming
        // the same parameter again keeps it, so that a sender can select
        // again and adjust the fine byte alone.
        if (self.registered, self.rpn, self.nrpn) != before {
            self.data_msb = None;
        }
    }

    /// Does what "reset all controllers" makes a MIDI 1.0 receiver do to
    /// its parameter numbers (RP-015): both are set to null, so that data
    /// entry changes nothing until a number is sent again. The bank is not
    /// among the things that message resets.
    fn deselect(&mut self) {
        self.registered = None;
        self.rpn = [Some(0x7F); 2];
        self.nrpn = [Some(0x7F); 2];
        self.data_msb = None;
    }
}

/// Up to two MIDI 2.0 channel voice messages, held without allocating:
/// what one MIDI 1.0 message becomes in [`Midi1ToMidi2::translate`].
///
/// Most messages become one. Some become none, because they are folded
/// into a later message, and one that arrives while a coarse data byte is
/// being held becomes two: the held parameter first, then itself.
///
/// ```
/// use manguebyte_midi::translate::Midi1ToMidi2;
/// use manguebyte_midi::MidiMessage;
///
/// let mut translator = Midi1ToMidi2::new();
/// let list = translator.translate(MidiMessage::ChannelPressure { channel: 0, pressure: 64 });
/// assert_eq!(list.len(), 1);
/// // By reference or by value, in the order the messages must be sent.
/// for message in &list {
///     assert_eq!(message.channel(), 0);
/// }
/// let mut out = Vec::new();
/// out.extend(list);
/// assert_eq!(out.len(), 1);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Midi2List {
    /// Entries past `len` are always [`MIDI2_FILLER`], so derived equality
    /// is exact.
    messages: [ChannelVoice2; 2],
    len: u8,
}

/// What unused entries of a [`Midi2List`] hold. Never visible from
/// outside.
const MIDI2_FILLER: ChannelVoice2 = ChannelVoice2::NoteOff {
    channel: 0,
    note: 0,
    velocity: 0,
    attribute: NoteAttribute::NONE,
};

impl Midi2List {
    /// The most messages a list holds.
    pub const CAPACITY: usize = 2;

    /// An empty list.
    pub const fn new() -> Self {
        Self {
            messages: [MIDI2_FILLER; 2],
            len: 0,
        }
    }

    /// The messages, in the order they must be sent.
    #[inline]
    pub fn as_slice(&self) -> &[ChannelVoice2] {
        &self.messages[..usize::from(self.len)]
    }

    /// Number of messages.
    #[inline]
    pub fn len(&self) -> usize {
        usize::from(self.len)
    }

    /// Whether the list holds no messages.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Iterates over the messages in order.
    pub fn iter(&self) -> core::slice::Iter<'_, ChannelVoice2> {
        self.as_slice().iter()
    }

    /// Adds a message at the end, if there is one to add. A third message
    /// would be dropped; the translator never produces one.
    fn push(&mut self, message: Option<ChannelVoice2>) {
        if let (Some(message), Some(slot)) = (message, self.messages.get_mut(usize::from(self.len)))
        {
            *slot = message;
            self.len += 1;
        }
    }
}

impl Default for Midi2List {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> IntoIterator for &'a Midi2List {
    type Item = &'a ChannelVoice2;
    type IntoIter = core::slice::Iter<'a, ChannelVoice2>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl IntoIterator for Midi2List {
    type Item = ChannelVoice2;
    type IntoIter = core::iter::Take<core::array::IntoIter<ChannelVoice2, 2>>;

    fn into_iter(self) -> Self::IntoIter {
        self.messages.into_iter().take(usize::from(self.len))
    }
}

/// Translates a stream of MIDI 1.0 channel voice messages into MIDI 2.0
/// messages, following the default translation of section D.3.
///
/// One translator serves the sixteen channels of one port or group. Most
/// messages translate one to one. These do not:
///
/// - **Note-on with velocity zero** becomes a note-off with velocity zero.
/// - **Bank select** (controllers 0 and 32) produces nothing; the bank is
///   remembered and travels with every later program change. Once either
///   controller has been received the bank counts as known, with zero for
///   a half that was never sent.
/// - **Parameter numbers** (controllers 98 to 101) produce nothing. The
///   null parameter and data with no complete parameter number are
///   dropped.
/// - A **fine data byte** (controller 38) produces a registered or
///   assignable controller message, with the 14-bit value made of the
///   coarse byte sent before it and itself. The coarse byte is remembered
///   for as long as the same parameter stays selected, so the fine byte
///   can be adjusted alone afterwards, also when the parameter's number
///   is sent again first. It is forgotten when another parameter is
///   selected and when an increment or decrement (controllers 96 and 97,
///   which pass through as controllers) moves the value; a fine byte that
///   arrives without it cannot be translated and is dropped.
/// - A **coarse data byte** (controller 6) is held, because a fine byte
///   may follow and the two are one message in MIDI 2.0. If the next
///   message given to the translator is that fine byte, they become one
///   message. Anything else sends the held byte first, with a fine byte
///   of zero, and is then translated itself, so [`translate`] can return
///   two messages. Nothing that arrives after a parameter is sent before
///   it: the MPE configuration message and the usual pitch bend
///   sensitivity message, which have no fine byte, reach the receiver
///   ahead of the notes that follow them. Only one coarse byte is ever
///   held, and the last message of a stream has nothing behind it to send
///   it: call [`flush`] when the input pauses, for instance at the end of
///   every audio block. The specification suggests a short timeout for
///   the same purpose.
/// - **Reset all controllers** (controller 121) is translated as the
///   controller it is. The translator also forgets the parameter number,
///   because RP-015 has a MIDI 1.0 receiver set it to null: data entry
///   that follows without a new number changes nothing in MIDI 1.0 and is
///   not translated. The bank is kept.
///
/// [`translate`]: Self::translate
/// [`flush`]: Self::flush
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Midi1ToMidi2 {
    channels: [ChannelState; 16],
    /// The channel whose coarse data byte is waiting for its fine byte.
    /// Never more than one, because any other message sends it.
    held: Option<u8>,
}

impl Midi1ToMidi2 {
    /// A translator that has seen nothing yet.
    pub const fn new() -> Self {
        Self {
            channels: [ChannelState::NEW; 16],
            held: None,
        }
    }

    /// Forgets remembered banks, parameter numbers and held data.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Takes the next MIDI 1.0 message and returns the MIDI 2.0 messages
    /// to send in its place, in order: none, one or two.
    ///
    /// ```
    /// use manguebyte_midi::translate::Midi1ToMidi2;
    /// use manguebyte_midi::ump::ChannelVoice2;
    /// use manguebyte_midi::MidiMessage;
    ///
    /// let cc = |controller, value| MidiMessage::ControlChange { channel: 0, controller, value };
    /// let mut translator = Midi1ToMidi2::new();
    /// // Pitch bend sensitivity of 12 semitones, sent the usual way: no
    /// // fine byte. Nothing can be sent yet.
    /// for message in [cc(101, 0), cc(100, 0), cc(6, 12)] {
    ///     assert!(translator.translate(message).is_empty());
    /// }
    /// // The note that follows comes out behind the parameter.
    /// let out = translator.translate(MidiMessage::NoteOn { channel: 0, note: 60, velocity: 100 });
    /// assert!(matches!(
    ///     out.as_slice(),
    ///     [
    ///         ChannelVoice2::RegisteredController { bank: 0, index: 0, .. },
    ///         ChannelVoice2::NoteOn { .. }
    ///     ]
    /// ));
    /// ```
    pub fn translate(&mut self, message: MidiMessage) -> Midi2List {
        let mut list = Midi2List::new();
        let channel = message.channel() & 0x0F;
        // The fine byte that completes the held coarse byte is the one
        // message that does not send it on its own.
        let completes = self.held == Some(channel)
            && matches!(
                message,
                MidiMessage::ControlChange { controller, .. } if controller & 0x7F == 38
            );
        if !completes {
            list.push(self.flush());
        }
        list.push(self.translate_one(message));
        list
    }

    /// Sends the coarse data byte that is being held, if there is one,
    /// with a fine byte of zero. To be called when no more input is
    /// expected for a while, so that a parameter sent without a fine byte
    /// does not wait for the next message.
    pub fn flush(&mut self) -> Option<ChannelVoice2> {
        let channel = self.held.take()?;
        self.channels[usize::from(channel)].coarse_alone(channel)
    }

    /// The message [`flush`](Self::flush) would send now, without sending
    /// it.
    pub fn held(&self) -> Option<ChannelVoice2> {
        let channel = self.held?;
        self.channels[usize::from(channel)].coarse_alone(channel)
    }

    fn control_change(&mut self, channel: u8, number: u8, value: u8) -> Option<ChannelVoice2> {
        let state = &mut self.channels[usize::from(channel)];
        let unchanged = ChannelVoice2::ControlChange {
            channel,
            controller: number,
            value: scale_7_to_32(value),
        };
        match number {
            // Bank select waits for the program change (section D.3.3).
            0 | 32 => {
                let bank = state.bank.get_or_insert(Bank { msb: 0, lsb: 0 });
                if number == 0 {
                    bank.msb = value;
                } else {
                    bank.lsb = value;
                }
                None
            }
            // Data entry, coarse byte: held until the next message shows
            // whether a fine byte belongs to it. Without a parameter to
            // set it means nothing and is not kept.
            6 => {
                state.data_msb = Some(value);
                if state.coarse_alone(channel).is_some() {
                    self.held = Some(channel);
                } else {
                    state.data_msb = None;
                }
                None
            }
            // Data entry, fine byte: completes the value. If a coarse byte
            // was being held it is this channel's, since any other message
            // would have sent it.
            38 => {
                self.held = None;
                state.controller(
                    channel,
                    (u16::from(state.data_msb?) << 7) | u16::from(value),
                )
            }
            98..=101 => {
                state.select(number, value);
                None
            }
            // Increment and decrement are plain controllers in MIDI 2.0
            // (section D.3.3). The receiver moves the value by a step of
            // its own choosing, so the coarse byte is no longer known.
            96 | 97 => {
                state.data_msb = None;
                Some(unchanged)
            }
            121 => {
                state.deselect();
                Some(unchanged)
            }
            // Everything else, including both halves of 14-bit
            // controllers, is translated one to one.
            _ => Some(unchanged),
        }
    }

    /// The translation of one message on its own, with no regard for a
    /// held coarse byte.
    fn translate_one(&mut self, message: MidiMessage) -> Option<ChannelVoice2> {
        Some(match message {
            MidiMessage::NoteOff {
                channel,
                note,
                velocity,
            } => ChannelVoice2::NoteOff {
                channel: channel & 0x0F,
                note: note & 0x7F,
                velocity: scale_7_to_16(velocity),
                attribute: NoteAttribute::NONE,
            },
            MidiMessage::NoteOn {
                channel,
                note,
                velocity,
            } => {
                let (channel, note) = (channel & 0x0F, note & 0x7F);
                if velocity & 0x7F == 0 {
                    // Section D.3.1.
                    ChannelVoice2::NoteOff {
                        channel,
                        note,
                        velocity: 0,
                        attribute: NoteAttribute::NONE,
                    }
                } else {
                    ChannelVoice2::NoteOn {
                        channel,
                        note,
                        velocity: scale_7_to_16(velocity),
                        attribute: NoteAttribute::NONE,
                    }
                }
            }
            MidiMessage::PolyPressure {
                channel,
                note,
                pressure,
            } => ChannelVoice2::PolyPressure {
                channel: channel & 0x0F,
                note: note & 0x7F,
                pressure: scale_7_to_32(pressure),
            },
            MidiMessage::ControlChange {
                channel,
                controller,
                value,
            } => {
                return self.control_change(channel & 0x0F, controller & 0x7F, value & 0x7F);
            }
            MidiMessage::ProgramChange { channel, program } => {
                let channel = channel & 0x0F;
                ChannelVoice2::ProgramChange {
                    channel,
                    program: program & 0x7F,
                    bank: self.channels[usize::from(channel)].bank,
                }
            }
            MidiMessage::ChannelPressure { channel, pressure } => ChannelVoice2::ChannelPressure {
                channel: channel & 0x0F,
                pressure: scale_7_to_32(pressure),
            },
            MidiMessage::PitchBend { channel, value } => ChannelVoice2::PitchBend {
                channel: channel & 0x0F,
                value: scale_14_to_32((i32::from(value) + 8192).clamp(0, 16_383) as u16),
            },
        })
    }
}

impl Default for Midi1ToMidi2 {
    fn default() -> Self {
        Self::new()
    }
}

/// Translates one MIDI 2.0 channel voice message into the MIDI 1.0
/// messages that stand for it, following the default translation of
/// section D.2.
///
/// - Notes, pressure, controllers and pitch bend become one message, with
///   the value cut to 7 or 14 bits. A note-on whose velocity would become
///   zero gets velocity 1, so that it is not taken for a note-off.
/// - A program change becomes the two bank select controllers followed by
///   the program change if it names a bank, and the program change alone
///   if not.
/// - A registered or assignable controller becomes four controller
///   messages: the two halves of the parameter number, then the coarse and
///   fine data bytes.
/// - Per-note controllers, per-note pitch bend, per-note management and
///   relative controllers have no MIDI 1.0 form: the list is empty.
/// - A control change with index 0, 6, 32, 38 or 98 to 101 translates to
///   nothing. In MIDI 1.0 those controllers are the pieces of bank select
///   and of parameter sequences; MIDI 2.0 has whole messages for both,
///   tells senders not to use the indexes and tells receivers to ignore
///   them (section 7.4.6). Passed on, they would change the bank or
///   rewrite whichever parameter the MIDI 1.0 receiver has selected.
///
/// ```
/// use manguebyte_midi::translate::midi2_to_midi1;
/// use manguebyte_midi::ump::ChannelVoice2;
/// use manguebyte_midi::MidiMessage;
///
/// // Pitch bend sensitivity of two semitones, as one MIDI 2.0 message.
/// let range = ChannelVoice2::RegisteredController {
///     channel: 0,
///     bank: 0,
///     index: 0,
///     value: 2 << 25,
/// };
/// let cc = |controller, value| MidiMessage::ControlChange { channel: 0, controller, value };
/// assert_eq!(
///     midi2_to_midi1(range).as_slice(),
///     [cc(101, 0), cc(100, 0), cc(6, 2), cc(38, 0)]
/// );
/// ```
pub fn midi2_to_midi1(message: ChannelVoice2) -> MessageList {
    let cc = |channel: u8, controller: u8, value: u8| MidiMessage::ControlChange {
        channel,
        controller,
        value,
    };
    let parameter = |channel: u8, (msb, lsb): (u8, u8), bank: u8, index: u8, value: u32| {
        // Figure 97: the top seven bits of the value are the coarse data
        // byte and the next seven the fine one.
        let value = scale_32_to_14(value);
        [
            cc(channel, msb, bank & 0x7F),
            cc(channel, lsb, index & 0x7F),
            cc(channel, 6, (value >> 7) as u8),
            cc(channel, 38, (value & 0x7F) as u8),
        ]
    };
    match message {
        ChannelVoice2::NoteOff {
            channel,
            note,
            velocity,
            ..
        } => MessageList::from_slice(&[MidiMessage::NoteOff {
            channel,
            note,
            velocity: scale_16_to_7(velocity),
        }]),
        ChannelVoice2::NoteOn {
            channel,
            note,
            velocity,
            ..
        } => MessageList::from_slice(&[MidiMessage::NoteOn {
            channel,
            note,
            // Section D.2.1.
            velocity: scale_16_to_7(velocity).max(1),
        }]),
        ChannelVoice2::PolyPressure {
            channel,
            note,
            pressure,
        } => MessageList::from_slice(&[MidiMessage::PolyPressure {
            channel,
            note,
            pressure: scale_32_to_7(pressure),
        }]),
        ChannelVoice2::ControlChange {
            channel,
            controller,
            value,
        } => match controller & 0x7F {
            // Section 7.4.6: these indexes are not used in MIDI 2.0 and a
            // receiver ignores them.
            0 | 6 | 32 | 38 | 98..=101 => MessageList::new(),
            controller => MessageList::from_slice(&[cc(channel, controller, scale_32_to_7(value))]),
        },
        ChannelVoice2::RegisteredController {
            channel,
            bank,
            index,
            value,
        } => MessageList::from_slice(&parameter(channel, (101, 100), bank, index, value)),
        ChannelVoice2::AssignableController {
            channel,
            bank,
            index,
            value,
        } => MessageList::from_slice(&parameter(channel, (99, 98), bank, index, value)),
        ChannelVoice2::ProgramChange {
            channel,
            program,
            bank,
        } => {
            let program = MidiMessage::ProgramChange {
                channel,
                program: program & 0x7F,
            };
            match bank {
                None => MessageList::from_slice(&[program]),
                // Section D.2.4: most significant byte, least significant
                // byte, program.
                Some(bank) => MessageList::from_slice(&[
                    cc(channel, 0, bank.msb & 0x7F),
                    cc(channel, 32, bank.lsb & 0x7F),
                    program,
                ]),
            }
        }
        ChannelVoice2::ChannelPressure { channel, pressure } => {
            MessageList::from_slice(&[MidiMessage::ChannelPressure {
                channel,
                pressure: scale_32_to_7(pressure),
            }])
        }
        ChannelVoice2::PitchBend { channel, value } => {
            MessageList::from_slice(&[MidiMessage::PitchBend {
                channel,
                value: scale_32_to_14(value) as i16 - 8192,
            }])
        }
        ChannelVoice2::RegisteredPerNoteController { .. }
        | ChannelVoice2::AssignablePerNoteController { .. }
        | ChannelVoice2::PerNoteManagement { .. }
        | ChannelVoice2::RelativeRegisteredController { .. }
        | ChannelVoice2::RelativeAssignableController { .. }
        | ChannelVoice2::PerNotePitchBend { .. } => MessageList::new(),
    }
}

/// Translates packets from the MIDI 1.0 protocol to the MIDI 2.0 protocol,
/// with a [`Midi1ToMidi2`] for each of the sixteen groups.
///
/// ```
/// use manguebyte_midi::translate::UmpTranslator;
/// use manguebyte_midi::ump::Packet;
///
/// let mut translator = UmpTranslator::new();
/// // A MIDI 1.0 note-on, velocity 100, as one word...
/// let midi1 = Packet::from_words(&[0x2390_3C64]).unwrap();
/// // ...becomes a MIDI 2.0 note-on of two words with a 16-bit velocity.
/// let midi2 = translator.to_midi2(midi1);
/// assert_eq!(midi2.len(), 1);
/// assert_eq!(midi2.as_slice()[0].words(), &[0x4390_3C00, 0xC924_0000]);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UmpTranslator {
    groups: [Midi1ToMidi2; 16],
}

impl UmpTranslator {
    /// A translator that has seen nothing yet.
    pub const fn new() -> Self {
        Self {
            groups: [const { Midi1ToMidi2::new() }; 16],
        }
    }

    /// Forgets everything remembered for every group.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Takes the next packet of a stream in the MIDI 1.0 protocol and
    /// returns the packets to send in its place in the MIDI 2.0 protocol,
    /// in order: none, one or two.
    ///
    /// MIDI 1.0 channel voice packets are translated as [`Midi1ToMidi2`]
    /// describes. Every other packet is the same in both protocols and is
    /// returned unchanged, as is a channel voice packet with a status that
    /// is not defined.
    ///
    /// A coarse data byte held for a group is sent ahead of the next
    /// packet addressed to that group, whatever that packet is, so that
    /// nothing overtakes it. The timing messages among the system
    /// real-time messages are the exception: clock, start, continue, stop
    /// and active sensing may arrive between any two messages and concern
    /// no channel, so they pass without sending it. Utility and stream
    /// messages are addressed to no group and do not send it either. A
    /// jitter-reduction timestamp is such a utility message; the
    /// translator passes it on as it is and does not reckon with it.
    ///
    /// A system reset message puts a MIDI 1.0 receiver back in its
    /// power-on state. It is passed on, behind anything held, and the
    /// translator then forgets what it remembered for that group, as
    /// [`Midi1ToMidi2::reset`] does.
    pub fn to_midi2(&mut self, packet: Packet) -> PacketList {
        let mut list = PacketList::new();
        match packet.message() {
            Ok(Message::Midi1 { group, message }) => {
                for message in self.groups[usize::from(group & 0x0F)].translate(message) {
                    list.push(Message::Midi2 { group, message }.to_packet());
                }
            }
            Ok(Message::System {
                message: SystemMessage::Reset,
                group,
            }) => {
                if let Some(held) = self.flush(group) {
                    list.push(held);
                }
                list.push(packet);
                self.groups[usize::from(group & 0x0F)].reset();
            }
            Ok(Message::System { message, .. }) if message.is_real_time() => list.push(packet),
            _ => {
                if let Some(held) = packet.group().and_then(|group| self.flush(group)) {
                    list.push(held);
                }
                list.push(packet);
            }
        }
        list
    }

    /// Sends the coarse data byte held for `group`, if there is one. See
    /// [`Midi1ToMidi2::flush`].
    pub fn flush(&mut self, group: u8) -> Option<Packet> {
        let group = group & 0x0F;
        self.groups[usize::from(group)]
            .flush()
            .map(|message| Message::Midi2 { group, message }.to_packet())
    }

    /// Sends every held coarse data byte: at most one packet for each
    /// group, passed to `send` in the order of the groups. To be called
    /// when the input pauses, for instance at the end of every audio
    /// block. Real-time safe as long as `send` is.
    pub fn flush_all(&mut self, mut send: impl FnMut(Packet)) {
        for group in 0..16 {
            if let Some(packet) = self.flush(group) {
                send(packet);
            }
        }
    }

    /// Whether a coarse data byte is being held for any group, so that
    /// [`flush_all`](Self::flush_all) would send something.
    pub fn is_holding(&self) -> bool {
        self.groups.iter().any(|group| group.held().is_some())
    }
}

impl Default for UmpTranslator {
    fn default() -> Self {
        Self::new()
    }
}

/// Up to four packets, held without allocating: what one packet becomes
/// when it is translated from one protocol to the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketList {
    /// Entries past `len` are always a no-op packet, so derived equality
    /// is exact.
    packets: [Packet; 4],
    len: u8,
}

impl PacketList {
    const fn new() -> Self {
        Self {
            packets: [Packet::one(0); 4],
            len: 0,
        }
    }

    /// Adds a packet at the end. A fifth would be dropped; nothing in this
    /// module produces one.
    fn push(&mut self, packet: Packet) {
        if let Some(slot) = self.packets.get_mut(usize::from(self.len)) {
            *slot = packet;
            self.len += 1;
        }
    }

    /// The packets, in the order they must be sent.
    pub fn as_slice(&self) -> &[Packet] {
        &self.packets[..usize::from(self.len)]
    }

    /// Number of packets.
    pub fn len(&self) -> usize {
        usize::from(self.len)
    }

    /// Whether there are no packets.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Iterates over the packets in order.
    pub fn iter(&self) -> core::slice::Iter<'_, Packet> {
        self.as_slice().iter()
    }
}

impl<'a> IntoIterator for &'a PacketList {
    type Item = &'a Packet;
    type IntoIter = core::slice::Iter<'a, Packet>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl IntoIterator for PacketList {
    type Item = Packet;
    type IntoIter = core::iter::Take<core::array::IntoIter<Packet, 4>>;

    fn into_iter(self) -> Self::IntoIter {
        self.packets.into_iter().take(usize::from(self.len))
    }
}

/// Translates one packet of a stream in the MIDI 2.0 protocol into the
/// packets to send in its place in the MIDI 1.0 protocol.
///
/// A MIDI 2.0 channel voice packet becomes the MIDI 1.0 channel voice
/// packets that [`midi2_to_midi1`] gives, which may be none. One with the
/// reserved opcode is dropped. Every other packet is the same in both
/// protocols and is returned unchanged.
pub fn packet_to_midi1(packet: Packet) -> PacketList {
    let mut list = PacketList::new();
    match packet.message() {
        Ok(Message::Midi2 { group, message }) => {
            for message in &midi2_to_midi1(message) {
                list.push(Packet::from_midi1(group, *message));
            }
        }
        _ if packet.message_type() == 0x4 => {}
        _ => list.push(packet),
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cc(channel: u8, controller: u8, value: u8) -> MidiMessage {
        MidiMessage::ControlChange {
            channel,
            controller,
            value,
        }
    }

    /// Checks the rules of section D.1.2 for every value of a source
    /// width, and returns how far the result strays, at worst, from an
    /// ideal straight line through minimum, centre and maximum, in units
    /// of one source step.
    fn check_scaling(from: u32, to: u32) -> f64 {
        let max_in = mask(from);
        let max_out = mask(to);
        let centre_in = 1u32 << (from - 1);
        let centre_out = 1u32 << (to - 1);
        // Minimum to minimum, centre to centre, maximum to maximum.
        assert_eq!(scale_up(0, from, to), 0, "{from} to {to}");
        assert_eq!(scale_up(centre_in, from, to), centre_out, "{from} to {to}");
        assert_eq!(scale_up(max_in, from, to), max_out, "{from} to {to}");

        let step = f64::from(1u32 << (to - from));
        let mut worst = 0.0f64;
        let mut previous = None;
        for value in 0..=max_in {
            let up = scale_up(value, from, to);
            // Strictly rising: no two inputs share an output.
            if let Some(previous) = previous {
                assert!(up > previous, "{from} to {to} at {value}");
            }
            previous = Some(up);
            // Narrowing undoes widening.
            assert_eq!(scale_down(up, to, from), value, "{from} to {to} at {value}");

            let ideal = if value <= centre_in {
                f64::from(value) * f64::from(centre_out) / f64::from(centre_in)
            } else {
                f64::from(centre_out)
                    + f64::from(value - centre_in) * f64::from(max_out - centre_out)
                        / f64::from(max_in - centre_in)
            };
            worst = worst.max((f64::from(up) - ideal).abs() / step);
        }
        worst
    }

    #[test]
    fn scaling_keeps_minimum_centre_and_maximum_for_every_7_bit_value() {
        // Exhaustive for the three conversions the protocols use. The
        // results stay within a hundredth of a source step of the straight
        // line; measured, the worst is 0.0015 of a step, for 7 to 16 bits.
        assert!(check_scaling(7, 16) < 0.01);
        assert!(check_scaling(7, 32) < 0.01);
        assert!(check_scaling(14, 32) < 0.01);
        // The shorthands are the general function at fixed widths.
        for value in 0..=127u8 {
            assert_eq!(
                u32::from(scale_7_to_16(value)),
                scale_up(value.into(), 7, 16)
            );
            assert_eq!(scale_7_to_32(value), scale_up(value.into(), 7, 32));
            assert_eq!(scale_16_to_7(scale_7_to_16(value)), value);
            assert_eq!(scale_32_to_7(scale_7_to_32(value)), value);
            // Section D.1.3 gives 7 to 16 bits as one expression.
            let shifted = u16::from(value) << 9;
            let repeat = u16::from(value) & 0x3F;
            let expected = if value <= 64 {
                shifted
            } else {
                shifted | (repeat << 3) | (repeat >> 3)
            };
            assert_eq!(scale_7_to_16(value), expected);
        }
        for value in 0..=0x3FFFu16 {
            assert_eq!(scale_14_to_32(value), scale_up(value.into(), 14, 32));
            assert_eq!(scale_32_to_14(scale_14_to_32(value)), value);
        }
    }

    #[test]
    fn scaling_examples_of_the_specification() {
        // Section D.1.3, numerical examples.
        assert_eq!(scale_7_to_16(10), 0x1400);
        assert_eq!(scale_7_to_16(64), 0x8000);
        assert_eq!(scale_7_to_16(87), 0xAEBA);
        assert_eq!(scale_7_to_16(127), 0xFFFF);
        // Section D.1.4.
        assert_eq!(scale_16_to_7(0x1400), 0x0A);
        assert_eq!(scale_16_to_7(0x8000), 0x40);
        assert_eq!(scale_16_to_7(0xAEBA), 0x57);
        assert_eq!(scale_16_to_7(0xFFFF), 0x7F);
        // Section D.3.1: "MIDI Velocity = 0x01: translates to 0x0200".
        assert_eq!(scale_7_to_16(1), 0x0200);
        // Table 23, centre values.
        assert_eq!(scale_7_to_32(0x40), 0x8000_0000);
        assert_eq!(scale_14_to_32(0x2000), 0x8000_0000);
        assert_eq!(scale_7_to_32(0x7F), 0xFFFF_FFFF);
        assert_eq!(scale_14_to_32(0x3FFF), 0xFFFF_FFFF);
    }

    #[test]
    fn scaling_holds_for_every_pair_of_widths() {
        let mut worst = 0.0f64;
        for from in 2..=16 {
            for to in from + 1..=32 {
                worst = worst.max(check_scaling(from, to));
            }
        }
        // Never further than half a source step from the straight line.
        // Measured, the worst is a quarter of a step, when a single bit is
        // added to 16.
        assert!(worst < 0.5, "worst deviation {worst}");
    }

    #[test]
    fn scaling_tolerates_any_widths() {
        // Same width: unchanged. Narrower target: narrowed.
        assert_eq!(scale_up(0x55, 7, 7), 0x55);
        assert_eq!(scale_up(0xFFFF, 16, 7), 0x7F);
        assert_eq!(scale_down(0x7F, 7, 16), 0xFFFF);
        assert_eq!(scale_down(0xABCD, 16, 16), 0xABCD);
        // Values wider than the source are masked.
        assert_eq!(scale_up(0xFF, 7, 16), 0xFFFF);
        assert_eq!(scale_down(0x1_FFFF, 16, 7), 0x7F);
        // Widths outside 1..=32 are clamped instead of overflowing a
        // shift.
        assert_eq!(scale_up(1, 0, 40), 0x8000_0000);
        assert_eq!(scale_up(0xFFFF_FFFF, 40, 99), 0xFFFF_FFFF);
        assert_eq!(scale_down(0xFFFF_FFFF, 99, 0), 1);
        assert_eq!(scale_up(1, 1, 32), 0x8000_0000);
        for from in 0..40 {
            for to in 0..40 {
                let _ = scale_up(0xFFFF_FFFF, from, to);
                let _ = scale_down(0xFFFF_FFFF, from, to);
            }
        }
    }

    /// What one message becomes, where that is at most one message.
    fn one(translator: &mut Midi1ToMidi2, message: MidiMessage) -> Option<ChannelVoice2> {
        let list = translator.translate(message);
        assert!(list.len() <= 1, "{message:?} became {list:?}");
        list.as_slice().first().copied()
    }

    /// The same for a packet.
    fn one_packet(translator: &mut UmpTranslator, packet: Packet) -> Option<Packet> {
        let list = translator.to_midi2(packet);
        assert!(list.len() <= 1, "{packet:?} became {list:?}");
        list.as_slice().first().copied()
    }

    /// Translates to MIDI 2.0 and back, expecting one message each way.
    fn there_and_back(message: MidiMessage) -> MidiMessage {
        let midi2 = one(&mut Midi1ToMidi2::new(), message)
            .unwrap_or_else(|| panic!("{message:?} was held"));
        let back = midi2_to_midi1(midi2);
        assert_eq!(back.len(), 1, "{message:?} became {back:?}");
        back.as_slice()[0]
    }

    #[test]
    fn midi1_survives_the_trip_to_midi2_and_back() {
        // Section D.1.2 asks for this; here it is checked for every value
        // of every message, on the first and the last channel.
        let mut checked = 0u32;
        for channel in [0u8, 15] {
            for data in 0..=127u8 {
                for note in [0u8, 60, 127] {
                    let mut messages = vec![
                        MidiMessage::NoteOff {
                            channel,
                            note,
                            velocity: data,
                        },
                        MidiMessage::PolyPressure {
                            channel,
                            note,
                            pressure: data,
                        },
                    ];
                    if data > 0 {
                        messages.push(MidiMessage::NoteOn {
                            channel,
                            note,
                            velocity: data,
                        });
                    }
                    for message in messages {
                        assert_eq!(there_and_back(message), message);
                        checked += 1;
                    }
                }
                for controller in (0..=127u8).filter(|c| !matches!(c, 0 | 6 | 32 | 38 | 98..=101)) {
                    let message = cc(channel, controller, data);
                    assert_eq!(there_and_back(message), message);
                    checked += 1;
                }
                for message in [
                    MidiMessage::ProgramChange {
                        channel,
                        program: data,
                    },
                    MidiMessage::ChannelPressure {
                        channel,
                        pressure: data,
                    },
                ] {
                    assert_eq!(there_and_back(message), message);
                    checked += 1;
                }
            }
            for value in -8192..=8191i16 {
                let message = MidiMessage::PitchBend { channel, value };
                assert_eq!(there_and_back(message), message);
                checked += 1;
            }
        }
        assert_eq!(checked, 2 * (127 * 9 + 6 + 128 * 120 + 128 * 2 + 16_384));
    }

    #[test]
    fn notes_follow_the_special_rules_for_velocity_zero() {
        let mut translator = Midi1ToMidi2::new();
        // Section D.3.1: a MIDI 1.0 note-on with velocity zero is a
        // MIDI 2.0 note-off with velocity zero. `MidiMessage::from_bytes`
        // already reads it as a note-off; a note-on built by hand with
        // velocity zero is treated the same.
        let off = ChannelVoice2::NoteOff {
            channel: 1,
            note: 60,
            velocity: 0,
            attribute: NoteAttribute::NONE,
        };
        let translator = &mut translator;
        assert_eq!(
            one(translator, MidiMessage::from_bytes(&[0x91, 60, 0]).unwrap()),
            Some(off)
        );
        assert_eq!(
            one(
                translator,
                MidiMessage::NoteOn {
                    channel: 1,
                    note: 60,
                    velocity: 0
                }
            ),
            Some(off)
        );
        assert_eq!(
            one(
                translator,
                MidiMessage::NoteOn {
                    channel: 1,
                    note: 60,
                    velocity: 1
                }
            ),
            Some(ChannelVoice2::NoteOn {
                channel: 1,
                note: 60,
                velocity: 0x0200,
                attribute: NoteAttribute::NONE,
            })
        );

        // Section D.2.1: a MIDI 2.0 note-on is never turned into velocity
        // zero. Velocities below 0x0200 would scale to zero and become 1.
        for velocity in [0u16, 1, 0x01FF, 0x0200, 0x03FF] {
            let back = midi2_to_midi1(ChannelVoice2::NoteOn {
                channel: 1,
                note: 60,
                velocity,
                attribute: NoteAttribute::pitch(60.5),
            });
            assert_eq!(
                back.as_slice(),
                [MidiMessage::NoteOn {
                    channel: 1,
                    note: 60,
                    velocity: 1
                }]
            );
        }
        // A note-off may have velocity zero.
        assert_eq!(
            midi2_to_midi1(off).as_slice(),
            [MidiMessage::NoteOff {
                channel: 1,
                note: 60,
                velocity: 0
            }]
        );
    }

    #[test]
    fn bank_select_travels_with_the_program_change() {
        let mut translator = Midi1ToMidi2::new();
        let program = |program| MidiMessage::ProgramChange {
            channel: 4,
            program,
        };
        let translator = &mut translator;
        // Figure 106: no bank known, bank-valid off.
        assert_eq!(
            one(translator, program(9)),
            Some(ChannelVoice2::ProgramChange {
                channel: 4,
                program: 9,
                bank: None
            })
        );
        // Bank select alone produces nothing (section D.3.3).
        assert_eq!(one(translator, cc(4, 0, 0x11)), None);
        assert_eq!(one(translator, cc(4, 32, 0x22)), None);
        // Figure 107.
        let with_bank = ChannelVoice2::ProgramChange {
            channel: 4,
            program: 10,
            bank: Some(Bank {
                msb: 0x11,
                lsb: 0x22,
            }),
        };
        assert_eq!(one(translator, program(10)), Some(with_bank));
        // The bank stays selected for later program changes, as it does in
        // a MIDI 1.0 receiver, and other channels have their own.
        assert!(matches!(
            one(translator, program(11)),
            Some(ChannelVoice2::ProgramChange { bank: Some(_), .. })
        ));
        let program_on = |channel, program| MidiMessage::ProgramChange { channel, program };
        assert_eq!(
            one(translator, program_on(5, 1)),
            Some(ChannelVoice2::ProgramChange {
                channel: 5,
                program: 1,
                bank: None
            })
        );
        // Only the most significant byte sent: the other half is zero.
        assert_eq!(one(translator, cc(6, 0, 3)), None);
        assert_eq!(
            one(translator, program_on(6, 2)),
            Some(ChannelVoice2::ProgramChange {
                channel: 6,
                program: 2,
                bank: Some(Bank { msb: 3, lsb: 0 })
            })
        );
        // Reset all controllers leaves the bank alone (RP-015).
        assert!(one(translator, cc(6, 121, 0)).is_some());
        assert!(matches!(
            one(translator, program_on(6, 3)),
            Some(ChannelVoice2::ProgramChange { bank: Some(_), .. })
        ));

        // Section D.2.4: bank most significant byte, least significant
        // byte, then the program.
        assert_eq!(
            midi2_to_midi1(with_bank).as_slice(),
            [cc(4, 0, 0x11), cc(4, 32, 0x22), program(10)]
        );
    }

    #[test]
    fn parameter_sequences_become_one_message() {
        let mut translator = Midi1ToMidi2::new();
        let feed = |translator: &mut Midi1ToMidi2, messages: &[(u8, u8)]| -> Vec<ChannelVoice2> {
            messages
                .iter()
                .flat_map(|&(controller, value)| translator.translate(cc(2, controller, value)))
                .collect()
        };
        let rpn = |index, value| ChannelVoice2::RegisteredController {
            channel: 2,
            bank: 0,
            index,
            value: scale_14_to_32(value),
        };

        // Figure 105: the message is sent when the fine byte arrives, with
        // the 14-bit value widened to 32 bits.
        assert_eq!(
            feed(&mut translator, &[(101, 0), (100, 0), (6, 12), (38, 34)]),
            [rpn(0, (12 << 7) | 34)]
        );
        // A fine byte alone adjusts under the coarse byte already given.
        assert_eq!(feed(&mut translator, &[(38, 35)]), [rpn(0, (12 << 7) | 35)]);

        // A device that sends only coarse bytes: each is held until the
        // next message shows it was complete.
        assert_eq!(feed(&mut translator, &[(6, 2)]), []);
        assert_eq!(translator.held(), Some(rpn(0, 2 << 7)));
        assert_eq!(feed(&mut translator, &[(6, 3)]), [rpn(0, 2 << 7)]);
        assert_eq!(feed(&mut translator, &[(100, 1)]), [rpn(0, 3 << 7)]);
        // Nothing is waiting now.
        assert_eq!(translator.held(), None);
        assert_eq!(translator.flush(), None);
        // The held byte can be asked for without waiting.
        assert_eq!(feed(&mut translator, &[(6, 64)]), []);
        assert_eq!(translator.held(), Some(rpn(1, 64 << 7)));
        assert_eq!(translator.flush(), Some(rpn(1, 64 << 7)));
        assert_eq!(translator.flush(), None);
        // After a flush, the next coarse byte does not send it again.
        assert_eq!(feed(&mut translator, &[(6, 65)]), []);
        // A fine byte completes a held coarse byte: one message, not two.
        assert_eq!(feed(&mut translator, &[(38, 1)]), [rpn(1, (65 << 7) | 1)]);
        assert_eq!(translator.held(), None);
        assert_eq!(feed(&mut translator, &[(101, 0)]), []);

        // Non-registered numbers become assignable controllers.
        assert_eq!(
            feed(&mut translator, &[(99, 5), (98, 6), (6, 127), (38, 127)]),
            [ChannelVoice2::AssignableController {
                channel: 2,
                bank: 5,
                index: 6,
                value: 0xFFFF_FFFF
            }]
        );
    }

    #[test]
    fn data_entry_without_a_valid_parameter_is_not_translated() {
        let feed = |messages: &[(u8, u8)]| -> Vec<ChannelVoice2> {
            let mut translator = Midi1ToMidi2::new();
            let mut out: Vec<ChannelVoice2> = messages
                .iter()
                .flat_map(|&(controller, value)| translator.translate(cc(0, controller, value)))
                .collect();
            out.extend(translator.flush());
            out
        };
        // No parameter number at all.
        assert_eq!(feed(&[(6, 1), (38, 2), (6, 3)]), []);
        // Only one half of the number.
        assert_eq!(feed(&[(101, 0), (6, 1), (38, 2)]), []);
        assert_eq!(feed(&[(98, 0), (6, 1), (38, 2)]), []);
        // The null function, registered and non-registered.
        assert_eq!(feed(&[(101, 127), (100, 127), (6, 1), (38, 2)]), []);
        assert_eq!(feed(&[(99, 127), (98, 127), (6, 1), (38, 2)]), []);
        // A fine byte with no coarse byte for this parameter.
        assert_eq!(feed(&[(101, 0), (100, 0), (38, 2)]), []);
        // The coarse byte of the previous parameter is not reused.
        assert_eq!(
            feed(&[(101, 0), (100, 0), (6, 1), (38, 2), (100, 1), (38, 3)]).len(),
            1
        );
        // 127 in one half only is an ordinary number.
        assert_eq!(feed(&[(101, 127), (100, 0), (6, 1)]).len(), 1);
    }

    #[test]
    fn a_held_parameter_is_sent_before_whatever_follows_it() {
        // The MPE configuration message and the usual pitch bend
        // sensitivity message have no fine byte. Whatever comes next, on
        // any channel, must not reach the receiver first.
        let held = ChannelVoice2::RegisteredController {
            channel: 0,
            bank: 0,
            index: 6,
            value: scale_14_to_32(15 << 7),
        };
        let followers = [
            MidiMessage::NoteOn {
                channel: 1,
                note: 60,
                velocity: 100,
            },
            MidiMessage::NoteOff {
                channel: 0,
                note: 60,
                velocity: 0,
            },
            MidiMessage::PolyPressure {
                channel: 9,
                note: 1,
                pressure: 2,
            },
            MidiMessage::ChannelPressure {
                channel: 0,
                pressure: 3,
            },
            MidiMessage::PitchBend {
                channel: 15,
                value: -5,
            },
            MidiMessage::ProgramChange {
                channel: 0,
                program: 4,
            },
            cc(0, 7, 100),
            cc(0, 96, 0),
            cc(0, 121, 0),
            cc(3, 64, 127),
            // A fine byte on another channel completes nothing here.
            cc(1, 38, 5),
        ];
        for follower in followers {
            let mut translator = Midi1ToMidi2::new();
            for part in [cc(0, 100, 6), cc(0, 101, 0), cc(0, 6, 15)] {
                assert!(translator.translate(part).is_empty());
            }
            let alone = Midi1ToMidi2::new().translate(follower);
            let out = translator.translate(follower);
            assert_eq!(out.as_slice().first(), Some(&held), "{follower:?}");
            assert_eq!(out.as_slice()[1..], *alone.as_slice(), "{follower:?}");
            // Sent once: nothing is left to flush.
            assert_eq!(translator.flush(), None);
        }

        // Messages that produce nothing themselves still send it: bank
        // select, a parameter number, another coarse byte.
        for (follower, still_held) in [
            (cc(4, 0, 1), false),
            (cc(4, 32, 1), false),
            (cc(0, 101, 0), false),
            (cc(5, 99, 1), false),
            (cc(0, 6, 14), true),
        ] {
            let mut translator = Midi1ToMidi2::new();
            for part in [cc(0, 100, 6), cc(0, 101, 0), cc(0, 6, 15)] {
                translator.translate(part);
            }
            assert_eq!(translator.translate(follower).as_slice(), [held]);
            assert_eq!(translator.held().is_some(), still_held, "{follower:?}");
        }

        // Only one coarse byte is ever held: a second, on another channel,
        // sends the first.
        let mut translator = Midi1ToMidi2::new();
        let mut out = Vec::new();
        for message in [
            cc(0, 101, 0),
            cc(0, 100, 0),
            cc(1, 101, 0),
            cc(1, 100, 0),
            cc(0, 6, 2),
            cc(1, 6, 48),
            // The fine byte for channel 0 arrives late: the coarse byte
            // has been sent, and is sent again with it, now complete.
            cc(0, 38, 50),
        ] {
            out.extend(translator.translate(message));
        }
        let range = |channel, value: u16| ChannelVoice2::RegisteredController {
            channel,
            bank: 0,
            index: 0,
            value: scale_14_to_32(value),
        };
        assert_eq!(
            out,
            [range(0, 2 << 7), range(1, 48 << 7), range(0, (2 << 7) | 50)]
        );
        assert_eq!(translator.flush(), None);
    }

    #[test]
    fn a_fine_byte_needs_a_coarse_byte_sent_for_the_same_selection() {
        let feed = |translator: &mut Midi1ToMidi2, messages: &[(u8, u8)]| -> Vec<u16> {
            messages
                .iter()
                .flat_map(|&(controller, value)| translator.translate(cc(1, controller, value)))
                .filter_map(|message| match message {
                    ChannelVoice2::RegisteredController {
                        bank: 0,
                        index: 0,
                        value,
                        ..
                    } => Some(scale_32_to_14(value)),
                    _ => None,
                })
                .collect()
        };
        let mut translator = Midi1ToMidi2::new();
        assert_eq!(
            feed(&mut translator, &[(101, 0), (100, 0), (6, 24), (38, 0)]),
            [24 << 7]
        );
        // MIDI 1.0: "a subsequent fine adjustment only requires the
        // sending of the LSB". The same parameter named again keeps the
        // coarse byte (section D.3.3: the translator holds the latest
        // value of controller 6).
        assert_eq!(
            feed(&mut translator, &[(101, 0), (100, 0), (38, 50)]),
            [(24 << 7) | 50]
        );
        assert_eq!(
            feed(&mut translator, &[(100, 0), (38, 51)]),
            [(24 << 7) | 51]
        );
        // Through the null parameter, or another parameter, the coarse
        // byte is gone and the fine byte alone cannot be sent.
        assert_eq!(
            feed(
                &mut translator,
                &[(101, 127), (100, 127), (101, 0), (100, 0), (38, 52)]
            ),
            []
        );
        assert_eq!(
            feed(&mut translator, &[(6, 12), (38, 53)]),
            [(12 << 7) | 53]
        );
        assert_eq!(feed(&mut translator, &[(100, 1), (100, 0), (38, 54)]), []);
        // An increment or decrement moves the value by a step only the
        // receiver knows.
        for step in [96u8, 97] {
            assert_eq!(
                feed(&mut translator, &[(6, 12), (step, 0), (38, 55)]),
                [12 << 7]
            );
        }
    }

    #[test]
    fn reset_all_controllers_deselects_the_parameter() {
        // RP-015: the parameter numbers go to null, so data entry after
        // the reset changes nothing in a MIDI 1.0 receiver, and must not
        // change anything behind the translator either.
        let mut translator = Midi1ToMidi2::new();
        let mut out = Vec::new();
        for message in [
            cc(0, 101, 0),
            cc(0, 100, 0),
            cc(0, 6, 7),
            cc(0, 121, 0),
            cc(0, 6, 12),
            cc(0, 38, 0),
            cc(0, 96, 0),
        ] {
            out.extend(translator.translate(message));
        }
        out.extend(translator.flush());
        let plain = |controller, value| ChannelVoice2::ControlChange {
            channel: 0,
            controller,
            value: scale_7_to_32(value),
        };
        assert_eq!(
            out,
            [
                // What was sent before the reset took effect before it.
                ChannelVoice2::RegisteredController {
                    channel: 0,
                    bank: 0,
                    index: 0,
                    value: scale_14_to_32(7 << 7),
                },
                plain(121, 0),
                plain(96, 0),
            ]
        );
        // Both numbers are null now, not unknown: one half sent names a
        // parameter whose other half is 127, as it does in a receiver.
        out.clear();
        for message in [cc(0, 101, 0), cc(0, 6, 1), cc(0, 38, 2)] {
            out.extend(translator.translate(message));
        }
        assert_eq!(
            out,
            [ChannelVoice2::RegisteredController {
                channel: 0,
                bank: 0,
                index: 127,
                value: scale_14_to_32((1 << 7) | 2),
            }]
        );
    }

    #[test]
    fn increment_decrement_and_14_bit_controllers_stay_separate() {
        let mut translator = Midi1ToMidi2::new();
        let translator = &mut translator;
        // Section D.3.3: increment and decrement are plain controllers in
        // MIDI 2.0, even while a parameter is selected.
        translator.translate(cc(0, 101, 0));
        translator.translate(cc(0, 100, 0));
        for controller in [96u8, 97] {
            assert_eq!(
                one(translator, cc(0, controller, 1)),
                Some(ChannelVoice2::ControlChange {
                    channel: 0,
                    controller,
                    value: scale_7_to_32(1)
                })
            );
        }
        // The two halves of a 14-bit controller are not merged.
        assert_eq!(
            one(translator, cc(0, 1, 127)),
            Some(ChannelVoice2::ControlChange {
                channel: 0,
                controller: 1,
                value: 0xFFFF_FFFF
            })
        );
        assert_eq!(
            one(translator, cc(0, 33, 64)),
            Some(ChannelVoice2::ControlChange {
                channel: 0,
                controller: 33,
                value: 0x8000_0000
            })
        );
    }

    #[test]
    fn midi2_controllers_that_stand_for_sequences_do_not_reach_midi1() {
        // Section 7.4.6: indexes 0, 6, 32, 38 and 98 to 101 are not used
        // in MIDI 2.0 and receivers ignore them. Every other index passes.
        for controller in 0..=255u8 {
            let back = midi2_to_midi1(ChannelVoice2::ControlChange {
                channel: 3,
                controller,
                value: 0x8000_0000,
            });
            let index = controller & 0x7F;
            if matches!(index, 0 | 6 | 32 | 38 | 98..=101) {
                assert!(back.is_empty(), "controller {controller} became {back:?}");
            } else {
                assert_eq!(back.as_slice(), [cc(3, index, 64)]);
            }
        }
        // The MIDI 1.0 side never produces them, so nothing is lost on the
        // way there and back: bank select and parameters travel as the
        // messages MIDI 2.0 has for them.
        let mut translator = Midi1ToMidi2::new();
        for controller in [0u8, 6, 32, 38, 98, 99, 100, 101] {
            for message in translator.translate(cc(3, controller, 1)) {
                assert!(
                    !matches!(message, ChannelVoice2::ControlChange { .. }),
                    "{message:?}"
                );
            }
        }
    }

    #[test]
    fn lists_of_messages_and_packets_iterate_in_order() {
        let mut translator = Midi1ToMidi2::new();
        assert_eq!(Midi2List::new(), Midi2List::default());
        assert!(Midi2List::new().is_empty());
        for part in [cc(0, 101, 0), cc(0, 100, 0), cc(0, 6, 2)] {
            assert_eq!(translator.translate(part), Midi2List::new());
        }
        let list = translator.translate(cc(0, 7, 1));
        assert_eq!(list.len(), Midi2List::CAPACITY);
        let by_reference: Vec<ChannelVoice2> = list.iter().copied().collect();
        let by_value: Vec<ChannelVoice2> = list.into_iter().collect();
        assert_eq!(by_reference, list.as_slice());
        assert_eq!(by_value, list.as_slice());
        assert_eq!((&list).into_iter().count(), 2);
        // Unused entries do not take part in equality.
        let one_message = translator.translate(cc(0, 7, 1));
        assert_eq!(one_message.len(), 1);
        assert_eq!(one_message, translator.translate(cc(0, 7, 1)));
        assert_ne!(one_message, list);

        let packets = packet_to_midi1(Packet::from_words(&[0x4F21_0000, 0x6000_0000]).unwrap());
        let by_value: Vec<Packet> = packets.into_iter().collect();
        assert_eq!(by_value, packets.as_slice());
        assert_eq!(by_value.len(), 4);
    }

    #[test]
    fn parameter_messages_expand_to_four_controllers_and_survive_the_trip() {
        // Figure 97.
        let registered = ChannelVoice2::RegisteredController {
            channel: 7,
            bank: 1,
            index: 2,
            value: 0x1234_5678,
        };
        // Top seven bits 0b0001001 = 9, next seven 0b0001101 = 13.
        assert_eq!(
            midi2_to_midi1(registered).as_slice(),
            [cc(7, 101, 1), cc(7, 100, 2), cc(7, 6, 9), cc(7, 38, 13)]
        );
        let assignable = ChannelVoice2::AssignableController {
            channel: 7,
            bank: 1,
            index: 2,
            value: 0xFFFF_FFFF,
        };
        assert_eq!(
            midi2_to_midi1(assignable).as_slice(),
            [cc(7, 99, 1), cc(7, 98, 2), cc(7, 6, 127), cc(7, 38, 127)]
        );

        // Every 14-bit value: MIDI 1.0 to 2.0 and back gives the same four
        // messages.
        let mut translator = Midi1ToMidi2::new();
        for value in 0..=0x3FFFu16 {
            for (msb, lsb) in [(101u8, 100u8), (99, 98)] {
                let sequence = [
                    cc(3, msb, 0x55),
                    cc(3, lsb, 0x2A),
                    cc(3, 6, (value >> 7) as u8),
                    cc(3, 38, (value & 0x7F) as u8),
                ];
                let midi2: Vec<ChannelVoice2> = sequence
                    .iter()
                    .flat_map(|message| translator.translate(*message))
                    .collect();
                assert_eq!(midi2.len(), 1);
                assert_eq!(midi2_to_midi1(midi2[0]).as_slice(), sequence);
            }
        }
    }

    #[test]
    fn messages_with_no_midi1_form_translate_to_nothing() {
        // Section D.2.8.
        for message in [
            ChannelVoice2::RegisteredPerNoteController {
                channel: 0,
                note: 60,
                controller: 3,
                value: 1,
            },
            ChannelVoice2::AssignablePerNoteController {
                channel: 0,
                note: 60,
                controller: 3,
                value: 1,
            },
            ChannelVoice2::PerNoteManagement {
                channel: 0,
                note: 60,
                detach: true,
                reset: true,
            },
            ChannelVoice2::RelativeRegisteredController {
                channel: 0,
                bank: 0,
                index: 0,
                delta: 1,
            },
            ChannelVoice2::RelativeAssignableController {
                channel: 0,
                bank: 0,
                index: 0,
                delta: -1,
            },
            ChannelVoice2::PerNotePitchBend {
                channel: 0,
                note: 60,
                value: 0,
            },
        ] {
            assert!(midi2_to_midi1(message).is_empty(), "{message:?}");
        }
    }

    #[test]
    fn values_are_narrowed_by_dropping_low_bits() {
        // Figures 95, 96 and 99.
        let one = |message| midi2_to_midi1(message).as_slice()[0];
        assert_eq!(
            one(ChannelVoice2::PolyPressure {
                channel: 1,
                note: 2,
                pressure: 0xFDFF_FFFF
            }),
            MidiMessage::PolyPressure {
                channel: 1,
                note: 2,
                pressure: 126
            }
        );
        assert_eq!(
            one(ChannelVoice2::ChannelPressure {
                channel: 1,
                pressure: 0x0200_0000
            }),
            MidiMessage::ChannelPressure {
                channel: 1,
                pressure: 1
            }
        );
        assert_eq!(
            one(ChannelVoice2::ControlChange {
                channel: 1,
                controller: 7,
                value: 0x8000_0000
            }),
            cc(1, 7, 64)
        );
        for (value, expected) in [
            (0u32, -8192i16),
            (0x7FFF_FFFF, -1),
            (0x8000_0000, 0),
            (0x8003_FFFF, 0),
            (0x8004_0000, 1),
            (0xFFFF_FFFF, 8191),
        ] {
            assert_eq!(
                one(ChannelVoice2::PitchBend { channel: 1, value }),
                MidiMessage::PitchBend {
                    channel: 1,
                    value: expected
                }
            );
        }
    }

    #[test]
    fn packets_translate_between_the_protocols_group_by_group() {
        let mut translator = UmpTranslator::new();
        let packet = |words: &[u32]| Packet::from_words(words).unwrap();

        let translator = &mut translator;

        // Note-on, velocity 100: 100 << 9 | 36 << 3 | 36 >> 3 = 0xC924.
        let midi2 = one_packet(translator, packet(&[0x2390_3C64])).unwrap();
        assert_eq!(midi2.words(), &[0x4390_3C00, 0xC924_0000]);
        assert_eq!(packet_to_midi1(midi2).as_slice(), [packet(&[0x2390_3C64])]);
        // Note-on with velocity zero: note-off with velocity zero.
        assert_eq!(
            one_packet(translator, packet(&[0x2390_3C00]))
                .unwrap()
                .words(),
            &[0x4380_3C00, 0x0000_0000]
        );

        // Groups keep their own state: a bank selected on group 1 does not
        // leak into group 2.
        assert_eq!(one_packet(translator, packet(&[0x21B0_0005])), None);
        assert_eq!(one_packet(translator, packet(&[0x21B0_2006])), None);
        assert_eq!(
            one_packet(translator, packet(&[0x22C0_0700]))
                .unwrap()
                .words(),
            &[0x42C0_0000, 0x0700_0000]
        );
        let with_bank = one_packet(translator, packet(&[0x21C0_0700])).unwrap();
        assert_eq!(with_bank.words(), &[0x41C0_0001, 0x0700_0506]);
        assert_eq!(
            packet_to_midi1(with_bank).as_slice(),
            [
                packet(&[0x21B0_0005]),
                packet(&[0x21B0_2006]),
                packet(&[0x21C0_0700])
            ]
        );

        // A held coarse data byte can be flushed for its group.
        assert!(!translator.is_holding());
        for word in [0x2FB1_6500u32, 0x2FB1_6400, 0x2FB1_0630] {
            assert_eq!(one_packet(translator, packet(&[word])), None);
        }
        assert!(translator.is_holding());
        assert_eq!(translator.flush(0), None);
        // Coarse byte 48 in the top seven bits of the 32-bit value.
        assert_eq!(
            translator.flush(15).unwrap().words(),
            &[0x4F21_0000, 0x6000_0000]
        );
        assert!(!translator.is_holding());
        // And expands to four packets on the way back.
        let four = packet_to_midi1(packet(&[0x4F21_0000, 0x6000_0000]));
        let words: Vec<u32> = four.iter().map(|p| p.words()[0]).collect();
        assert_eq!(words, [0x2FB1_6500, 0x2FB1_6400, 0x2FB1_0630, 0x2FB1_2600]);
        assert_eq!((&four).into_iter().count(), four.len());

        // Everything that is not channel voice is the same in both
        // protocols.
        let unchanged: [&[u32]; 6] = [
            &[0x0020_1234],
            &[0x10F8_0000],
            &[0x3004_7E7F, 0x0903_0000],
            &[0xD010_0000, 50_000_000, 0, 0],
            &[0xF020_0000, 0, 0, 0],
            &[0xB123_4567, 1, 2],
        ];
        for words in unchanged {
            let original = packet(words);
            assert_eq!(one_packet(translator, original), Some(original));
            assert_eq!(packet_to_midi1(original).as_slice(), [original]);
        }
        // MIDI 2.0 packets are already MIDI 2.0.
        assert_eq!(one_packet(translator, midi2), Some(midi2));
        // What has no MIDI 1.0 form, or no meaning at all, is dropped.
        assert!(packet_to_midi1(packet(&[0x4060_3C00, 0])).is_empty());
        assert!(packet_to_midi1(packet(&[0x4070_0000, 0])).is_empty());
        // A MIDI 1.0 packet with an undefined status passes untouched.
        let odd = packet(&[0x2000_0000]);
        assert_eq!(one_packet(translator, odd), Some(odd));

        translator.reset();
        assert_eq!(*translator, UmpTranslator::default());
    }

    #[test]
    fn a_held_parameter_goes_out_ahead_of_the_next_packet_of_its_group() {
        let packet = |words: &[u32]| Packet::from_words(words).unwrap();
        // The MPE configuration message on group 2, channel 0: registered
        // parameter 6 with a coarse byte of 15 and no fine byte.
        let configure = [0x22B0_6406u32, 0x22B0_6500, 0x22B0_060F];
        let held = packet(&[0x4220_0006, 15 << 25]);
        let holding = || {
            let mut translator = UmpTranslator::new();
            for word in configure {
                assert!(translator.to_midi2(packet(&[word])).is_empty());
            }
            assert!(translator.is_holding());
            translator
        };

        // A note on a member channel of the same group: the configuration
        // first, then the note.
        let mut translator = holding();
        let out = translator.to_midi2(packet(&[0x2291_3C64]));
        assert_eq!(out.as_slice(), [held, packet(&[0x4291_3C00, 0xC924_0000])]);
        assert!(!translator.is_holding());

        // Packets of the group that are not channel voice send it too, and
        // pass unchanged behind it: system exclusive data (which may reset
        // the device), a system common message, flex data, a MIDI 2.0
        // packet, a status this crate does not know.
        let others: [&[u32]; 6] = [
            &[0x3204_7E7F, 0x0901_0000],
            &[0x12F3_0500],
            &[0xD210_0000, 50_000_000, 0, 0],
            &[0x4291_3C00, 0xC924_0000],
            &[0x2200_0000],
            &[0x5201_0000, 0, 0, 0],
        ];
        for words in others {
            let mut translator = holding();
            let out = translator.to_midi2(packet(words));
            assert_eq!(out.as_slice(), [held, packet(words)], "{words:08X?}");
        }

        // What does not send it: system real-time messages, which may come
        // between any two messages; packets of other groups; and utility
        // and stream messages, which have no group.
        let mut translator = holding();
        let passing: [&[u32]; 9] = [
            &[0x12F8_0000],
            &[0x12FA_0000],
            &[0x12FC_0000],
            &[0x12FE_0000],
            &[0x2391_3C64],
            &[0x13F3_0500],
            &[0x0020_1234],
            &[0x0000_0000],
            &[0xF020_0000, 0, 0, 0],
        ];
        for words in passing {
            let out = translator.to_midi2(packet(words));
            assert_eq!(out.len(), 1, "{words:08X?}");
            assert!(translator.is_holding(), "{words:08X?}");
        }
        // So a fine byte that arrives after a clock tick still completes
        // the value: one packet, not two.
        let out = translator.to_midi2(packet(&[0x22B0_2632]));
        assert_eq!(
            out.as_slice(),
            [packet(&[0x4220_0006, ((15 << 7) | 50) << 18])]
        );
        assert!(!translator.is_holding());

        // A system reset is passed on behind what was held, and leaves
        // nothing remembered for its group: the receiver is back in its
        // power-on state, where no parameter is selected and no bank known.
        let mut translator = UmpTranslator::new();
        for word in [0x22B3_0005u32, configure[0], configure[1], configure[2]] {
            assert!(translator.to_midi2(packet(&[word])).is_empty());
        }
        let reset = packet(&[0x12FF_0000]);
        assert_eq!(translator.to_midi2(reset).as_slice(), [held, reset]);
        for word in [0x22B0_0601u32, 0x22B0_2602] {
            assert!(translator.to_midi2(packet(&[word])).is_empty());
        }
        assert!(!translator.is_holding());
        assert_eq!(
            translator.to_midi2(packet(&[0x22C3_0700])).as_slice(),
            [packet(&[0x42C3_0000, 0x0700_0000])]
        );
        assert_eq!(translator, UmpTranslator::new());

        // At the end of a block everything held is sent, group by group.
        let mut translator = UmpTranslator::new();
        for group in [9u32, 0, 4] {
            for low in [0x00B0_6500u32, 0x00B0_6400, 0x00B0_0602] {
                let word = 0x2000_0000 | (group << 24) | low;
                assert!(translator.to_midi2(packet(&[word])).is_empty());
            }
        }
        let mut flushed = Vec::new();
        translator.flush_all(|packet| flushed.push(packet.words()[0]));
        assert_eq!(flushed, [0x4020_0000, 0x4420_0000, 0x4920_0000]);
        assert!(!translator.is_holding());
        translator.flush_all(|packet| panic!("sent {packet:?} twice"));
    }
}
