//! Notes, expressions and MIDI that a plugin sends out.
//!
//! A plugin that makes notes, such as an arpeggiator, a sequencer or a
//! pitch tracker, says so with [`Plugin::NOTE_OUTPUT`](crate::Plugin::NOTE_OUTPUT)
//! and writes what it plays into the [`OutputBuffer`] that
//! [`Plugin::process_with_output`](crate::Plugin::process_with_output)
//! hands it, each event on the frame of the block it belongs to:
//!
//! - [`OutputKind::NoteOn`], [`NoteOff`](OutputKind::NoteOff),
//!   [`NoteChoke`](OutputKind::NoteChoke) and
//!   [`NoteExpression`](OutputKind::NoteExpression): the framework's own
//!   notes and per-note expressions, the same as a plugin receives. This is
//!   what most plugins need, and every format carries it well.
//! - [`OutputKind::Midi2`]: any MIDI 2.0 channel voice message, for what
//!   the framework's notes do not say: per-note controllers of every
//!   number, registered and assignable, note attributes of every type,
//!   per-note management, 16-bit velocity and 32-bit controllers.
//! - [`OutputKind::Midi1`]: a MIDI 1.0 channel message, as it is.
//!
//! Whatever runs the plugin turns these into what its host or port takes:
//! CLAP's notes and expressions or its MIDI events, VST 3's events and note
//! expressions, or MIDI for a port. For MIDI, [`OutputTranslator`] does it,
//! into MIDI 2.0 packets or into MIDI 1.0, plain or as MPE; see
//! [`OutputProtocol`] for what each keeps.
//!
//! Nothing here allocates after it is made, so all of it works on the
//! audio thread.
//!
//! ```
//! use manguebyte_dsp::event::{Expression, NoteRef};
//! use manguebyte_midi::ump::Message;
//! use manguebyte_plugin::output::{OutputBuffer, OutputProtocol, OutputTranslator};
//!
//! let mut output = OutputBuffer::with_capacity(16);
//! let note = NoteRef::new(0, 60);
//! output.note_on(0, note, 1.0);
//! output.expression(0, note, Expression::Tuning, 0.5);
//! output.note_off(100, note, 0.0);
//!
//! // For a MIDI 2.0 port: a note-on, its per-note pitch bend (after the
//! // range it is measured in) and a note-off.
//! let mut translator = OutputTranslator::new(OutputProtocol::Midi2);
//! let mut packets = Vec::new();
//! translator.translate_buffer(&output, |offset, packet| packets.push((offset, packet)));
//! assert_eq!(packets.len(), 4);
//! assert!(matches!(packets[0].1.message(), Ok(Message::Midi2 { .. })));
//! assert_eq!(packets[3].0, 100);
//! ```
//!
//! # Limits
//!
//! Tuning goes out as per-note pitch bend over ±48 semitones, which is
//! announced to the receiver with registered controller 0/7, and is
//! clamped to that range. Volume goes out as per-note controller 7, whose
//! scale stops at unity gain, so a louder volume is sent as unity. A note
//! choke has no MIDI form and is sent as a note-off with velocity zero.

use manguebyte_dsp::event::{Expression, NoteRef};
use manguebyte_midi::MidiMessage;
use manguebyte_midi::mpe::Zone;
use manguebyte_midi::translate::{Midi2ToMpe, UmpTranslator, midi2_to_midi1};
use manguebyte_midi::ump::{ChannelVoice2, Message, NoteAttribute, PITCH_BEND_CENTRE, Packet};

/// Something a plugin sends out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutputKind {
    /// A note starts.
    NoteOn {
        /// The note. Its identifier, if it has one, is how the events that
        /// follow find it; without one, channel and key do.
        note: NoteRef,
        /// How hard it is struck, from 0 to 1.
        velocity: f32,
    },
    /// A note is released.
    NoteOff {
        /// The note.
        note: NoteRef,
        /// How fast it is released, from 0 to 1.
        velocity: f32,
    },
    /// A note stops at once, without its release.
    NoteChoke {
        /// The note.
        note: NoteRef,
    },
    /// One note is shaped, in the unit documented on [`Expression`].
    NoteExpression {
        /// The note.
        note: NoteRef,
        /// Which dimension.
        expression: Expression,
        /// The new value.
        value: f64,
    },
    /// A MIDI 2.0 channel voice message.
    Midi2 {
        /// The UMP group, `0..=15`.
        group: u8,
        /// The message.
        message: ChannelVoice2,
    },
    /// A MIDI 1.0 channel message.
    Midi1 {
        /// The UMP group, `0..=15`, for a port that has groups.
        group: u8,
        /// The message.
        message: MidiMessage,
    },
}

/// Something a plugin sends out, and the frame of the block it falls on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutputEvent {
    /// Frames from the start of the block. One past the block's end or
    /// later is sent on its last frame.
    pub offset: u32,
    /// What is sent.
    pub kind: OutputKind,
}

/// What a plugin sends out during one block, in time order, with room set
/// aside up front.
///
/// Events are kept in the order of their frames, and in the order they
/// were added within a frame. [`push`](Self::push) refuses an event, and
/// hands it back, once the buffer is full, rather than allocating on the
/// audio thread. A buffer made with [`new`](Self::new) has no room at all,
/// which is what a plugin that does not set
/// [`Plugin::NOTE_OUTPUT`](crate::Plugin::NOTE_OUTPUT) is given.
#[derive(Debug, Default, PartialEq)]
pub struct OutputBuffer {
    events: Vec<OutputEvent>,
    capacity: usize,
}

impl OutputBuffer {
    /// A buffer with no room. Does not allocate.
    pub const fn new() -> Self {
        Self {
            events: Vec::new(),
            capacity: 0,
        }
    }

    /// A buffer with room for `capacity` events. Allocates.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            events: Vec::with_capacity(capacity),
            capacity,
        }
    }

    /// Adds an event in time order, or hands it back if the buffer is
    /// full. Real-time safe.
    pub fn push(&mut self, event: OutputEvent) -> Result<(), OutputEvent> {
        if self.events.len() >= self.capacity {
            return Err(event);
        }
        // Events nearly always arrive in order, so look from the back.
        let index = self
            .events
            .iter()
            .rposition(|existing| existing.offset <= event.offset)
            .map_or(0, |position| position + 1);
        self.events.insert(index, event);
        Ok(())
    }

    /// Adds `kind` on frame `offset`. Returns whether there was room.
    pub fn send(&mut self, offset: u32, kind: OutputKind) -> bool {
        self.push(OutputEvent { offset, kind }).is_ok()
    }

    /// Starts a note. Returns whether there was room.
    pub fn note_on(&mut self, offset: u32, note: NoteRef, velocity: f32) -> bool {
        self.send(offset, OutputKind::NoteOn { note, velocity })
    }

    /// Releases a note. Returns whether there was room.
    pub fn note_off(&mut self, offset: u32, note: NoteRef, velocity: f32) -> bool {
        self.send(offset, OutputKind::NoteOff { note, velocity })
    }

    /// Shapes a sounding note. Returns whether there was room.
    pub fn expression(
        &mut self,
        offset: u32,
        note: NoteRef,
        expression: Expression,
        value: f64,
    ) -> bool {
        self.send(
            offset,
            OutputKind::NoteExpression {
                note,
                expression,
                value,
            },
        )
    }

    /// Sends a MIDI 2.0 message on group 0. Returns whether there was room.
    pub fn midi2(&mut self, offset: u32, message: ChannelVoice2) -> bool {
        self.send(offset, OutputKind::Midi2 { group: 0, message })
    }

    /// Sends a MIDI 1.0 message on group 0. Returns whether there was room.
    pub fn midi1(&mut self, offset: u32, message: MidiMessage) -> bool {
        self.send(offset, OutputKind::Midi1 { group: 0, message })
    }

    /// Removes every event, keeping the room.
    pub fn clear(&mut self) {
        self.events.clear();
    }

    /// How many events there are.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// How many events fit.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// The events, in time order.
    pub fn as_slice(&self) -> &[OutputEvent] {
        &self.events
    }

    /// Iterates over the events in time order.
    pub fn iter(&self) -> core::slice::Iter<'_, OutputEvent> {
        self.events.iter()
    }
}

impl<'a> IntoIterator for &'a OutputBuffer {
    type Item = &'a OutputEvent;
    type IntoIter = core::slice::Iter<'a, OutputEvent>;

    fn into_iter(self) -> Self::IntoIter {
        self.events.iter()
    }
}

/// Which note each identifier names, for events that name a note by its
/// identifier alone.
///
/// CLAP and VST 3 hosts address expressions and note-offs by identifier,
/// with the key left out, and a plugin that answers in kind does the same.
/// MIDI and some hosts need the channel and key, so whatever sends the
/// plugin's notes on keeps one of these: [`start`](Self::start) on each
/// note-on, [`resolve`](Self::resolve) for each event about a note, and
/// [`end`](Self::end) on its note-off or choke.
///
/// It follows a fixed number of notes, chosen up front; past that, a new
/// note is not followed, and events that name it find it by the channel
/// and key they carry. After [`with_capacity`](Self::with_capacity), which
/// allocates, every method is real-time safe.
///
/// ```
/// use manguebyte_dsp::event::NoteRef;
/// use manguebyte_plugin::output::NoteIds;
///
/// let mut ids = NoteIds::with_capacity(8);
/// ids.start(NoteRef::with_id(2, 64, 9));
/// assert_eq!(ids.resolve(NoteRef::with_id(0, 0, 9)), NoteRef::with_id(2, 64, 9));
/// assert_eq!(ids.end(NoteRef::with_id(0, 0, 9)), NoteRef::with_id(2, 64, 9));
/// // Once ended, the identifier names nothing.
/// assert_eq!(ids.resolve(NoteRef::with_id(0, 0, 9)), NoteRef::with_id(0, 0, 9));
/// ```
#[derive(Debug, Clone, Default)]
pub struct NoteIds {
    /// Identifier, channel and key of each identified note that sounds.
    notes: Vec<(u32, u8, u8)>,
    capacity: usize,
}

impl NoteIds {
    /// Room to follow `capacity` notes at once. Allocates.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            notes: Vec::with_capacity(capacity),
            capacity,
        }
    }

    /// Records a note that starts. A note without an identifier needs
    /// nothing recorded.
    pub fn start(&mut self, note: NoteRef) {
        let Some(id) = note.id else {
            return;
        };
        self.forget(id);
        if self.notes.len() < self.capacity {
            self.notes.push((id, note.channel & 0x0F, note.key & 0x7F));
        }
    }

    /// `note` with the channel and key of the sounding note its
    /// identifier names, or as it is if it names none.
    pub fn resolve(&self, note: NoteRef) -> NoteRef {
        let found = note
            .id
            .and_then(|id| self.notes.iter().find(|(known, ..)| *known == id));
        match found {
            Some(&(id, channel, key)) => NoteRef::with_id(channel, key, id),
            None => NoteRef {
                channel: note.channel & 0x0F,
                key: note.key & 0x7F,
                id: note.id,
            },
        }
    }

    /// [`resolve`](Self::resolve), then forgets the note, which ends.
    pub fn end(&mut self, note: NoteRef) -> NoteRef {
        let resolved = self.resolve(note);
        if let Some(id) = note.id {
            self.forget(id);
        }
        resolved
    }

    /// Forgets every note.
    pub fn clear(&mut self) {
        self.notes.clear();
    }

    fn forget(&mut self, id: u32) {
        if let Some(index) = self.notes.iter().position(|(known, ..)| *known == id) {
            self.notes.swap_remove(index);
        }
    }
}

/// What a MIDI port or host on the other end speaks, and so what an
/// [`OutputTranslator`] writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutputProtocol {
    /// MIDI 2.0 channel voice messages, in Universal MIDI Packets of
    /// message type 4. Nothing is lost: notes carry 16-bit velocity,
    /// expressions become per-note messages, and MIDI 1.0 sent by the
    /// plugin is translated up by the specification's default translation.
    Midi2,
    /// MIDI 1.0, by the default translation of M2-104-UM (appendix D.2):
    /// values are cut to 7 and 14 bits, and what shapes one note
    /// other than polyphonic pressure is dropped (section D.2.8), since
    /// MIDI 1.0 has no message for it. Packets are of message type 2.
    Midi1,
    /// MIDI 1.0 as MPE in this zone, through [`Midi2ToMpe`]: each note
    /// gets a member channel, and its tuning, pressure and brightness
    /// become that channel's pitch bend, channel pressure and controller
    /// 74. Packets are of message type 2.
    Mpe(Zone),
}

/// Registered per-note controllers (M2-104-UM, section 7.4.4), as the
/// framework's input reads them.
mod per_note {
    pub const VOLUME: u8 = 7;
    pub const PAN: u8 = 10;
    pub const BRIGHTNESS: u8 = 74;
    pub const VIBRATO_DEPTH: u8 = 77;
}

/// The per-note pitch bend range the translator writes in, in semitones,
/// the default that MPE member channels have too.
pub const PER_NOTE_BEND_RANGE: u8 = 48;

/// How many sounding notes with identifiers an [`OutputTranslator`]
/// follows at once. Past that, a note's events find it by channel and key.
const IDENTIFIED_NOTES: usize = 256;

/// Turns a plugin's output into MIDI packets for a port or a host that
/// takes MIDI.
///
/// It keeps the state the translation needs across blocks: which
/// identifier names which note, which note numbers carry per-note values
/// that a new note must not inherit, and for MPE which member channel each
/// note is on. Keep one per stream of output, and feed it every block in
/// order.
///
/// For the framework's own notes and expressions:
///
/// - A note-on and a note-off become the MIDI 2.0 messages, with the
///   velocity at 16 bits; a choke becomes a note-off with velocity zero.
///   Before a note-on on a note number that an expression has shaped,
///   per-note management detaches the notes still sounding there and
///   resets the number's values, since MIDI 2.0 keeps per-note values per
///   note number and the new note would otherwise inherit them.
/// - [`Expression::Tuning`] becomes per-note pitch bend over
///   [`PER_NOTE_BEND_RANGE`] semitones, announced once per channel with
///   registered controller 0/7. [`Expression::Pressure`] becomes
///   polyphonic pressure, [`Expression::Brightness`] registered per-note
///   controller 74, [`Expression::Vibrato`] 77, [`Expression::Pan`] 10 and
///   [`Expression::Volume`] 7, read back as a gain of `(v / max)²`. These
///   are the inverse of how the framework's own input reads MIDI 2.0
///   (`manguebyte_dsp::note_input`), so what a plugin sends, another reads
///   as it was sent.
///
/// Then the protocol decides; see [`OutputProtocol`]. MIDI 2.0 messages
/// from the plugin go the same way, and MIDI 1.0 messages are translated
/// up for MIDI 2.0 and passed on unchanged for MIDI 1.0 and MPE.
///
/// After [`new`](Self::new), which allocates, every method is real-time
/// safe.
#[derive(Debug, Clone)]
pub struct OutputTranslator {
    protocol: OutputProtocol,
    group: u8,
    identified: NoteIds,
    /// Per channel, one bit per note number shaped since its last reset.
    shaped: [u128; 16],
    /// Per channel, one bit per note number on which a note sounds.
    sounding: [u128; 16],
    /// Channels whose per-note bend range has been announced.
    announced: u16,
    up: UmpTranslator,
    mpe: Option<Midi2ToMpe>,
}

impl OutputTranslator {
    /// A translator into `protocol`, sending the framework's notes on
    /// group 0. Allocates.
    pub fn new(protocol: OutputProtocol) -> Self {
        Self {
            protocol,
            group: 0,
            identified: NoteIds::with_capacity(IDENTIFIED_NOTES),
            shaped: [0; 16],
            sounding: [0; 16],
            announced: 0,
            up: UmpTranslator::new(),
            mpe: match protocol {
                OutputProtocol::Mpe(zone) => Some(Midi2ToMpe::new(zone)),
                _ => None,
            },
        }
    }

    /// The same translator, sending the framework's notes on `group`
    /// (`0..=15`) instead.
    #[must_use]
    pub fn with_group(mut self, group: u8) -> Self {
        self.group = group & 0x0F;
        self
    }

    /// What it writes.
    pub fn protocol(&self) -> OutputProtocol {
        self.protocol
    }

    /// Forgets every note and value, as at the start.
    pub fn reset(&mut self) {
        self.identified.clear();
        self.shaped = [0; 16];
        self.sounding = [0; 16];
        self.announced = 0;
        self.up.reset();
        if let Some(mpe) = &mut self.mpe {
            mpe.reset();
        }
    }

    /// Translates every event of a block, handing each packet to `emit`
    /// with the frame of the event it came from.
    pub fn translate_buffer(&mut self, buffer: &OutputBuffer, mut emit: impl FnMut(u32, Packet)) {
        for event in buffer {
            self.translate(&event.kind, |packet| emit(event.offset, packet));
        }
    }

    /// Translates one event, handing each packet it becomes to `emit` in
    /// the order they are to be sent.
    pub fn translate(&mut self, kind: &OutputKind, mut emit: impl FnMut(Packet)) {
        let group = self.group;
        match *kind {
            OutputKind::Midi2 { group, message } => self.write(group & 0x0F, message, &mut emit),
            OutputKind::Midi1 { group, message } => {
                let packet = Packet::from_midi1(group & 0x0F, message);
                match self.protocol {
                    OutputProtocol::Midi2 => {
                        for &packet in self.up.to_midi2(packet).as_slice() {
                            emit(packet);
                        }
                    }
                    OutputProtocol::Midi1 | OutputProtocol::Mpe(_) => emit(packet),
                }
            }
            OutputKind::NoteOn { note, velocity } => {
                let (channel, key) = (note.channel & 0x0F, note.key & 0x7F);
                self.identified.start(note);
                let bit = 1u128 << key;
                if self.shaped[usize::from(channel)] & bit != 0 {
                    self.shaped[usize::from(channel)] &= !bit;
                    let reset = ChannelVoice2::PerNoteManagement {
                        channel,
                        note: key,
                        detach: true,
                        reset: true,
                    };
                    self.write(group, reset, &mut emit);
                }
                let on = ChannelVoice2::NoteOn {
                    channel,
                    note: key,
                    velocity: velocity_16(velocity),
                    attribute: NoteAttribute::NONE,
                };
                self.write(group, on, &mut emit);
            }
            OutputKind::NoteOff { note, velocity } => {
                let NoteRef { channel, key, .. } = self.identified.end(note);
                let off = ChannelVoice2::NoteOff {
                    channel,
                    note: key,
                    velocity: velocity_16(velocity),
                    attribute: NoteAttribute::NONE,
                };
                self.write(group, off, &mut emit);
            }
            OutputKind::NoteChoke { note } => {
                let NoteRef { channel, key, .. } = self.identified.end(note);
                let off = ChannelVoice2::NoteOff {
                    channel,
                    note: key,
                    velocity: 0,
                    attribute: NoteAttribute::NONE,
                };
                self.write(group, off, &mut emit);
            }
            OutputKind::NoteExpression {
                note,
                expression,
                value,
            } => {
                let NoteRef { channel, key, .. } = self.identified.resolve(note);
                self.shaped[usize::from(channel)] |= 1u128 << key;
                let controller = |controller, value| ChannelVoice2::RegisteredPerNoteController {
                    channel,
                    note: key,
                    controller,
                    value,
                };
                let message = match expression {
                    Expression::Tuning => {
                        self.announce_range(channel, &mut emit);
                        ChannelVoice2::PerNotePitchBend {
                            channel,
                            note: key,
                            value: bipolar_32(value / f64::from(PER_NOTE_BEND_RANGE)),
                        }
                    }
                    Expression::Pressure => ChannelVoice2::PolyPressure {
                        channel,
                        note: key,
                        pressure: unit_32(value),
                    },
                    Expression::Brightness => {
                        controller(per_note::BRIGHTNESS, bipolar_32(2.0 * value - 1.0))
                    }
                    Expression::Vibrato => {
                        controller(per_note::VIBRATO_DEPTH, bipolar_32(2.0 * value - 1.0))
                    }
                    Expression::Pan => controller(per_note::PAN, bipolar_32(value)),
                    // The receiver squares it, as General MIDI's curve for
                    // controller 7 does.
                    Expression::Volume => {
                        controller(per_note::VOLUME, unit_32(value.max(0.0).sqrt()))
                    }
                };
                self.write(group, message, &mut emit);
            }
        }
    }

    /// Ends every note that the translator knows to be sounding, as a
    /// player stopping does, and forgets them. MIDI 1.0 notes the plugin
    /// sent itself to a MIDI 1.0 or MPE port are not followed and are not
    /// ended.
    pub fn end_all(&mut self, mut emit: impl FnMut(Packet)) {
        let group = self.group;
        if let Some(mpe) = &mut self.mpe {
            mpe.end_all(|message| emit(Packet::from_midi1(group, message)));
        } else {
            for channel in 0..16u8 {
                let mut keys = self.sounding[usize::from(channel)];
                while keys != 0 {
                    let key = keys.trailing_zeros() as u8;
                    keys &= !(1u128 << key);
                    let off = ChannelVoice2::NoteOff {
                        channel,
                        note: key,
                        velocity: 0,
                        attribute: NoteAttribute::NONE,
                    };
                    self.write(group, off, &mut emit);
                }
            }
        }
        self.sounding = [0; 16];
        self.identified.clear();
    }

    /// Sends one MIDI 2.0 message in the protocol.
    fn write(&mut self, group: u8, message: ChannelVoice2, emit: &mut impl FnMut(Packet)) {
        let channel = usize::from(message.channel() & 0x0F);
        match message {
            ChannelVoice2::NoteOn { note, .. } => self.sounding[channel] |= 1u128 << (note & 0x7F),
            ChannelVoice2::NoteOff { note, .. } => {
                self.sounding[channel] &= !(1u128 << (note & 0x7F));
            }
            _ => {}
        }
        match (&mut self.mpe, self.protocol) {
            (Some(mpe), _) => mpe.translate(message, |midi| emit(Packet::from_midi1(group, midi))),
            (None, OutputProtocol::Midi1) => {
                for &midi in midi2_to_midi1(message).as_slice() {
                    emit(Packet::from_midi1(group, midi));
                }
            }
            (None, _) => emit(Message::Midi2 { group, message }.to_packet()),
        }
    }

    /// Tells a MIDI 2.0 receiver, once per channel, the range per-note
    /// pitch bend is written in.
    fn announce_range(&mut self, channel: u8, emit: &mut impl FnMut(Packet)) {
        let bit = 1u16 << channel;
        if self.announced & bit != 0 || self.protocol != OutputProtocol::Midi2 {
            return;
        }
        self.announced |= bit;
        let range = ChannelVoice2::RegisteredController {
            channel,
            bank: 0,
            index: 7,
            value: u32::from(PER_NOTE_BEND_RANGE) << 25,
        };
        let group = self.group;
        self.write(group, range, emit);
    }
}

/// A velocity from 0 to 1 as sixteen bits.
fn velocity_16(velocity: f32) -> u16 {
    // `as` saturates, and maps a NaN to zero.
    (f64::from(velocity).clamp(0.0, 1.0) * 65_535.0).round() as u16
}

/// A value from 0 to 1 as thirty-two bits, all ones at 1.
fn unit_32(value: f64) -> u32 {
    (value.clamp(0.0, 1.0) * f64::from(u32::MAX)).round() as u32
}

/// A value from -1 to 1 as thirty-two bits with the centre at zero: the
/// minimum is -1 and the maximum exactly 1, as the min-centre-max scaling
/// of M2-104-UM (appendix D.1.3) places them.
fn bipolar_32(value: f64) -> u32 {
    let value = if value.is_nan() {
        0.0
    } else {
        value.clamp(-1.0, 1.0)
    };
    let centre = f64::from(PITCH_BEND_CENTRE);
    let steps = if value >= 0.0 {
        value * (f64::from(u32::MAX) - centre)
    } else {
        value * centre
    };
    (centre + steps.round()) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_reach_the_ends_and_the_centre() {
        assert_eq!(velocity_16(1.0), 0xFFFF);
        assert_eq!(velocity_16(0.0), 0);
        assert_eq!(velocity_16(f32::NAN), 0);
        assert_eq!(unit_32(1.0), u32::MAX);
        assert_eq!(unit_32(-3.0), 0);
        assert_eq!(bipolar_32(0.0), PITCH_BEND_CENTRE);
        assert_eq!(bipolar_32(1.0), u32::MAX);
        assert_eq!(bipolar_32(-1.0), 0);
        assert_eq!(bipolar_32(f64::NAN), PITCH_BEND_CENTRE);
        assert_eq!(bipolar_32(7.0), u32::MAX);
    }

    #[test]
    fn a_full_buffer_hands_the_event_back_and_order_is_kept() {
        let mut buffer = OutputBuffer::with_capacity(3);
        let note = |key| OutputKind::NoteOn {
            note: NoteRef::new(0, key),
            velocity: 1.0,
        };
        assert!(buffer.send(10, note(1)));
        assert!(buffer.send(0, note(2)));
        assert!(buffer.send(10, note(3)));
        assert!(!buffer.send(0, note(4)));
        let keys: Vec<u32> = buffer.iter().map(|event| event.offset).collect();
        assert_eq!(keys, [0, 10, 10]);
        assert_eq!(buffer.as_slice()[1].kind, note(1));
        assert!(!OutputBuffer::new().send(0, note(1)));
    }
}
