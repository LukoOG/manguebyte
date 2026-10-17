//! Turning MIDI 1.0, MPE and MIDI 2.0 into the framework's note events.
//!
//! A processor learns about notes from [`Event`]s: a [`NoteOn`] and a
//! [`NoteOff`] carrying a [`NoteRef`], and [`NoteExpression`]s that shape
//! one sounding note. Plugin formats that have notes of their own (CLAP,
//! VST 3) deliver them that way directly. MIDI does not: MIDI 1.0 bends a
//! whole channel, MPE gets per-note control out of it by giving each note a
//! channel of its own, and MIDI 2.0 addresses notes by number with messages
//! of thirty-two bits. [`NoteInput`] reads all three and produces the same
//! events from them, so a processor written against [`Expression`] plays
//! from any of them without knowing which it was.
//!
//! Every device input, the standalone runner, the plugin wrappers and the
//! test harness send their MIDI through a `NoteInput`, so there is one
//! reading of MIDI in the framework.
//!
//! # What becomes what
//!
//! The rule is that what applies to one note becomes a per-note event, and
//! what applies to a whole channel stays the MIDI 1.0 message it always
//! was, in [`EventKind::Midi`], for the processor to apply to every note on
//! that channel as it already does (a [`Synth`](crate::Synth) through its
//! own pitch-bend range).
//!
//! **Plain MIDI 1.0**, on a channel outside every MPE zone, is passed on
//! exactly as [`Event::from_midi`] reads it, with one addition: a
//! high-resolution velocity prefix (controller 88, MIDI CA-031) gives the
//! note-on or note-off that follows it fourteen bits of velocity. Notes
//! carry no identifier.
//!
//! **MPE** ("MIDI Polyphonic Expression" 1.0, RP-053). Once a zone is
//! configured, by the MPE configuration message in the stream or by
//! [`Mpe::Zones`]:
//!
//! - a note on a member channel is reported on its zone's **master
//!   channel**, with an identifier of its own. The member channel is how
//!   the sender reached the note, not what the note is: reported this way,
//!   the zone's own messages (the master channel's pitch bend, pedal and
//!   controllers) reach every note of the zone through the processor's
//!   ordinary channel handling, as MPE intends.
//! - the member channel's pitch bend becomes [`Expression::Tuning`], scaled
//!   by the zone's member bend range (48 semitones until registered
//!   parameter 0 says otherwise), its channel pressure
//!   [`Expression::Pressure`] and its controller 74
//!   [`Expression::Brightness`]. Values set before the note-on, as senders
//!   do, are the note's starting values.
//! - a note on the master channel is tracked the same way, and polyphonic
//!   key pressure on the master channel becomes the note's pressure.
//! - once a note's key is released it stops following its member channel
//!   (section 3.3.3), which may already belong to another note.
//!
//! **MIDI 2.0** channel voice messages (M2-104-UM, "Universal MIDI Packet
//! (UMP) Format and MIDI 2.0 Protocol" 1.1.1):
//!
//! - every note gets an identifier, and its sixteen-bit velocity; a pitch
//!   attribute (type 3, 7.9 fixed point) makes the note number an index
//!   and the attribute its pitch, reported as a [`Expression::Tuning`]
//!   away from the key.
//! - per-note pitch bend becomes [`Expression::Tuning`], scaled by the
//!   per-note bend range: 48 semitones until registered controller 0/7
//!   says otherwise. Polyphonic pressure becomes [`Expression::Pressure`].
//! - registered per-note controllers: #3 (pitch, 7.25 fixed point) moves
//!   the note's pitch as the attribute does, #74 is
//!   [`Expression::Brightness`], #77 (vibrato depth)
//!   [`Expression::Vibrato`], #10 [`Expression::Pan`], and #7 (volume) and
//!   #11 (expression) together [`Expression::Volume`], each as a gain of
//!   `(v / max)²`, the curve General MIDI recommends for controllers 7 and
//!   11 (40·log₁₀ of the fraction, in decibels).
//! - per-note management detaches the notes playing on a number from
//!   later per-note messages, or puts the number's per-note controllers
//!   back to their defaults.
//! - per-note values belong to the note number, not to one note: a value
//!   sent before a note-on is that note's starting value, and it stays for
//!   the next note on the number until it changes or is reset.
//! - everything that applies to the whole channel is turned into MIDI 1.0
//!   by the specification's default translation
//!   ([`midi2_to_midi1`]) and passed on.
//! - on a member channel of an MPE zone, MIDI 2.0 is read as MPE is, at
//!   its full resolution: that is MPE carried over MIDI 2.0.
//!
//! MIDI 1.0 values are first widened to MIDI 2.0's resolution with the
//! specification's own method ([`scale_up`], appendix D), so an MPE
//! stream and the MIDI 2.0 stream that says the same thing produce
//! identical events, bit for bit.
//!
//! Each event lands on the frame of the message it came from. A note's
//! identifier is the same on its note-on, every expression and its
//! note-off. Identifiers made here have their top bit set, so they never
//! equal the identifiers CLAP and VST 3 hosts give (non-negative 32-bit
//! integers), which pass through unchanged.
//!
//! [`NoteOn`]: EventKind::NoteOn
//! [`NoteOff`]: EventKind::NoteOff
//! [`NoteExpression`]: EventKind::NoteExpression
//! [`scale_up`]: manguebyte_midi::translate::scale_up
//! [`midi2_to_midi1`]: manguebyte_midi::translate::midi2_to_midi1
//!
//! ```
//! use manguebyte_dsp::event::{EventBuffer, EventKind, Expression};
//! use manguebyte_dsp::note_input::{Mpe, NoteInput};
//! use manguebyte_midi::MidiMessage;
//!
//! let mut input = NoteInput::new(Mpe::LOWER_ZONE);
//! let mut events = EventBuffer::with_capacity(16);
//! let mut feed = |offset, message| input.midi1(offset, message, |event| {
//!     let _ = events.push(event);
//! });
//! // Member channel 2 bends down by half of its 48 semitones, then
//! // plays middle C.
//! feed(0, MidiMessage::PitchBend { channel: 1, value: -4096 });
//! feed(10, MidiMessage::NoteOn { channel: 1, note: 60, velocity: 100 });
//!
//! let kinds: Vec<EventKind> = events.iter().map(|event| event.kind).collect();
//! let EventKind::NoteOn { note, .. } = kinds[0] else { panic!() };
//! // Reported on the zone's master channel, with an identifier.
//! assert_eq!((note.channel, note.key), (0, 60));
//! assert!(note.id.is_some());
//! let EventKind::NoteExpression { expression, value, .. } = kinds[1] else { panic!() };
//! assert_eq!(expression, Expression::Tuning);
//! assert_eq!(value, -24.0);
//! assert!(events.iter().all(|event| event.offset == 10));
//! ```
//!
//! # Limits
//!
//! Registered per-note controllers other than those above (modulation,
//! breath, the other sound controllers) and assignable per-note controllers
//! have no [`Expression`] to become, and are dropped. So are relative
//! controllers. Channel-wide MIDI 2.0 messages lose their extra resolution
//! on the way to MIDI 1.0. Messages of every UMP group are read as one
//! set of sixteen channels. Controllers on a member channel other than the
//! three MPE dimensions are passed on, on the member channel, where a
//! processor that keys controllers by channel finds no note: the note is
//! on the master channel. A note-off for a note that started before its
//! channel joined a zone is passed on as plain MIDI, so it still ends the
//! note it began. At most [`NoteInput::with_capacity`] notes are followed
//! at once; past that the oldest is released to make room.

use manguebyte_midi::MidiMessage;
use manguebyte_midi::mpe::{
    Configuration, MEMBER_PITCH_BEND_RANGE, Role, TIMBRE_CONTROLLER, ZoneKind, ZoneLayout,
};
use manguebyte_midi::rpn::{Change, Parameter, ParameterEvent, RpnDecoder};
use manguebyte_midi::translate::{midi2_to_midi1, scale_7_to_32, scale_14_to_32, scale_up};
use manguebyte_midi::ump::{ChannelVoice2, Message, Packet};

use crate::event::{Event, EventKind, Expression, NoteRef};

/// The centre of a 32-bit value: no bend, pan in the middle, a sound
/// controller at rest.
const CENTRE: u32 = 0x8000_0000;

/// The high-resolution velocity prefix (MIDI CA-031).
const VELOCITY_PREFIX: u8 = 88;
const ALL_SOUND_OFF: u8 = 120;
const RESET_ALL_CONTROLLERS: u8 = 121;
/// Controllers 123 to 127: all notes off, and the four mode messages that
/// include it.
const ALL_NOTES_OFF: u8 = 123;

/// Registered per-note controllers (M2-104-UM, section 7.4.4), numbered as
/// the MIDI 1.0 controllers they mirror.
mod per_note {
    pub const PITCH_7_25: u8 = 3;
    pub const VOLUME: u8 = 7;
    pub const PAN: u8 = 10;
    pub const EXPRESSION: u8 = 11;
    pub const BRIGHTNESS: u8 = 74;
    pub const VIBRATO_DEPTH: u8 = 77;
}

/// Registered controller 0/7: the range of per-note pitch bend. Its
/// default is 48 semitones, the same as MPE's member channels, so that
/// the two describe one bend alike.
const PER_NOTE_BEND_RANGE: (u8, u8) = (0, 7);

/// Default bend range of member channels and of per-note pitch bend, in
/// cents.
const DEFAULT_RANGE_CENTS: u16 = MEMBER_PITCH_BEND_RANGE as u16 * 100;

/// How many notes a [`NoteInput`] follows unless told otherwise.
pub const DEFAULT_CAPACITY: usize = 128;

/// How a [`NoteInput`] treats MPE.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Mpe {
    /// MIDI 1.0 is never read as MPE: configuration messages are passed on
    /// as the controllers they are and nothing else. MIDI 2.0 notes are
    /// still per-note, as MIDI 2.0 itself makes them.
    Off,
    /// No zones until the stream configures them with the MPE
    /// configuration message. This is the default: a stream that never
    /// sends one is plain MIDI.
    #[default]
    Follow,
    /// Start with these zones, given as numbers of member channels
    /// (`0..=15`, zero for none; the upper zone gives way where they
    /// overlap), and follow configuration messages from there. For an
    /// instrument that knows it is played from an MPE controller, since
    /// many hosts never pass the configuration message on.
    Zones {
        /// Member channels of the lower zone, whose master is channel 0.
        lower: u8,
        /// Member channels of the upper zone, whose master is channel 15.
        upper: u8,
    },
}

impl Mpe {
    /// One lower zone with all fifteen member channels: what MPE
    /// controllers send unless set up otherwise.
    pub const LOWER_ZONE: Self = Self::Zones {
        lower: 15,
        upper: 0,
    };

    /// The zones to start with.
    fn layout(self) -> ZoneLayout {
        let mut layout = ZoneLayout::new();
        if let Self::Zones { lower, upper } = self {
            layout.configure(Configuration {
                zone: ZoneKind::Upper,
                members: upper.min(15),
            });
            layout.configure(Configuration {
                zone: ZoneKind::Lower,
                members: lower.min(15),
            });
        }
        layout
    }
}

/// The per-note values of a note or a note number, at MIDI 2.0
/// resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Shape {
    /// Pitch bend, [`CENTRE`] for none.
    bend: u32,
    pressure: u32,
    /// Controller 74, [`CENTRE`] at rest.
    timbre: u32,
    volume: u32,
    expression: u32,
    pan: u32,
    vibrato: u32,
    /// The note's own pitch in semitones, 7.25 fixed point, when an
    /// attribute or per-note controller #3 has given one.
    pitch: Option<u32>,
}

impl Shape {
    /// What a note number holds before anything is sent, and after a
    /// per-note reset.
    const REST: Self = Self {
        bend: CENTRE,
        pressure: 0,
        timbre: CENTRE,
        volume: u32::MAX,
        expression: u32::MAX,
        pan: CENTRE,
        vibrato: CENTRE,
        pitch: None,
    };
}

/// The six expressions as last sent for a note, so that only changes are
/// sent again.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Sent([f64; 6]);

/// The order expressions are reported in, and their values at rest: what
/// a processor assumes for a note that has been told nothing.
const EXPRESSIONS: [(Expression, f64); 6] = [
    (Expression::Tuning, 0.0),
    (Expression::Pressure, 0.0),
    (Expression::Brightness, 0.5),
    (Expression::Volume, 1.0),
    (Expression::Pan, 0.0),
    (Expression::Vibrato, 0.5),
];

/// A note whose key is down.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Note {
    /// The identifier every event about the note carries.
    id: u32,
    /// The channel it arrived on.
    source: u8,
    key: u8,
    /// The channel it is reported on: the zone's master for a note that
    /// arrived on a member channel.
    reported: u8,
    /// Whether it arrived on a member channel, and so follows that
    /// channel's pitch bend, pressure and timbre.
    member: bool,
    /// Whether per-note management has detached it from its number.
    detached: bool,
    shape: Shape,
    sent: Sent,
}

impl Note {
    fn reference(&self) -> NoteRef {
        NoteRef::with_id(self.reported, self.key, self.id)
    }
}

/// What a [`NoteInput`] remembers about one channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Channel {
    /// Pitch bend, pressure and timbre of a member channel, at MIDI 2.0
    /// resolution. A note that arrives on the channel starts from them.
    bend: u32,
    pressure: u32,
    timbre: u32,
    /// What a full bend of a member channel amounts to, in cents.
    member_range: u16,
    /// What a full per-note pitch bend amounts to, in cents.
    per_note_range: u16,
    /// The low seven bits of the next note's velocity, from controller 88.
    velocity_lsb: Option<u8>,
}

impl Channel {
    const NEW: Self = Self {
        bend: CENTRE,
        pressure: 0,
        timbre: CENTRE,
        member_range: DEFAULT_RANGE_CENTS,
        per_note_range: DEFAULT_RANGE_CENTS,
        velocity_lsb: None,
    };
}

/// Where a MIDI 1.0 or 2.0 value goes, once it is known to address the
/// dimension of a member channel or a note number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dimension {
    Bend,
    Pressure,
    Timbre,
    Volume,
    Expression,
    Pan,
    Vibrato,
    Pitch,
}

/// Turns MIDI 1.0, MPE and MIDI 2.0 into note events. See the [module
/// documentation](self) for what becomes what.
///
/// One `NoteInput` reads one stream, and keeps the state that stream
/// builds up: the zones, each channel's MPE dimensions and bend ranges,
/// the per-note values of every note number, and the notes whose keys are
/// down. Every event it produces is handed to the `emit` function passed
/// in, on the frame given with the message.
///
/// # Real-time safety
///
/// [`new`](Self::new) and [`with_capacity`](Self::with_capacity)
/// allocate. Every other method is real-time safe: it neither allocates,
/// locks nor blocks, and does a bounded amount of work, as long as `emit`
/// does too (pushing to an [`EventBuffer`](crate::event::EventBuffer)
/// does).
#[derive(Debug, Clone, PartialEq)]
pub struct NoteInput {
    mpe: Mpe,
    layout: ZoneLayout,
    channels: [Channel; 16],
    /// The per-note values of every note number, channel by channel.
    keys: Vec<Shape>,
    /// Notes whose keys are down, oldest first.
    notes: Vec<Note>,
    capacity: usize,
    rpn: RpnDecoder,
    next_id: u32,
}

impl NoteInput {
    /// A converter that treats MPE as `mpe` says and follows up to
    /// [`DEFAULT_CAPACITY`] notes at once. Allocates.
    pub fn new(mpe: Mpe) -> Self {
        Self::with_capacity(mpe, DEFAULT_CAPACITY)
    }

    /// A converter that follows up to `notes` notes at once (at least
    /// one). Allocates.
    pub fn with_capacity(mpe: Mpe, notes: usize) -> Self {
        let capacity = notes.max(1);
        Self {
            mpe,
            layout: mpe.layout(),
            channels: [Channel::NEW; 16],
            keys: vec![Shape::REST; 16 * 128],
            notes: Vec::with_capacity(capacity),
            capacity,
            rpn: RpnDecoder::new(),
            next_id: 0,
        }
    }

    /// How MPE is treated.
    pub fn mpe(&self) -> Mpe {
        self.mpe
    }

    /// The MPE zones as they are now.
    pub fn layout(&self) -> ZoneLayout {
        self.layout
    }

    /// How many notes have their keys down.
    pub fn held_notes(&self) -> usize {
        self.notes.len()
    }

    /// Forgets every note without reporting it, and puts every channel and
    /// note number back to rest. The zones go back to what `mpe` starts
    /// with. Identifiers carry on from where they were, so a note started
    /// afterwards is never taken for one from before. Real-time safe.
    pub fn reset(&mut self) {
        self.layout = self.mpe.layout();
        self.channels = [Channel::NEW; 16];
        self.keys.fill(Shape::REST);
        self.notes.clear();
        self.rpn.reset();
    }

    /// Changes how MPE is treated, and moves to the zones `mpe` starts
    /// with. Notes on channels whose role changes are released, as a
    /// configuration message would release them. Real-time safe.
    pub fn set_mpe(&mut self, mpe: Mpe, offset: u32, mut emit: impl FnMut(Event)) {
        self.mpe = mpe;
        self.apply_layout(offset, mpe.layout(), None, &mut emit);
    }

    /// Reads one MIDI 1.0 message. Real-time safe.
    pub fn midi1(&mut self, offset: u32, message: MidiMessage, mut emit: impl FnMut(Event)) {
        let emit = &mut emit;
        if let Some(parameter) = self.rpn.feed(message) {
            self.parameter(offset, &parameter, emit);
        }
        let channel = message.channel() & 0x0F;
        let role = self.layout.role(channel);
        match message {
            MidiMessage::NoteOn { note, velocity, .. } if velocity & 0x7F == 0 => {
                self.midi1_note_off(offset, channel, note & 0x7F, 0, emit);
            }
            MidiMessage::NoteOn { note, velocity, .. } => {
                let velocity = self.velocity(channel, velocity);
                if role == Role::Outside {
                    plain_note(offset, channel, note & 0x7F, velocity, true, emit);
                } else {
                    let velocity = velocity.wide();
                    self.start(offset, channel, note & 0x7F, velocity, None, None, emit);
                }
            }
            MidiMessage::NoteOff { note, velocity, .. } => {
                self.midi1_note_off(offset, channel, note & 0x7F, velocity, emit);
            }
            MidiMessage::PolyPressure { note, pressure, .. } => match role {
                Role::Outside => emit(Event::from_midi(offset, message)),
                // Not allowed on member channels (section 2.5).
                Role::Member(_) => {}
                Role::Master(_) => {
                    let value = scale_7_to_32(pressure & 0x7F);
                    self.per_note(
                        offset,
                        channel,
                        note & 0x7F,
                        Dimension::Pressure,
                        value,
                        emit,
                    );
                }
            },
            MidiMessage::PitchBend { value, .. } if matches!(role, Role::Member(_)) => {
                let raw = (i32::from(value).clamp(-8192, 8191) + 8192) as u16;
                self.member(offset, channel, Dimension::Bend, scale_14_to_32(raw), emit);
            }
            MidiMessage::ChannelPressure { pressure, .. } if matches!(role, Role::Member(_)) => {
                let value = scale_7_to_32(pressure & 0x7F);
                self.member(offset, channel, Dimension::Pressure, value, emit);
            }
            MidiMessage::ControlChange {
                controller, value, ..
            } => self.midi1_controller(offset, message, channel, controller, value, role, emit),
            _ => emit(Event::from_midi(offset, message)),
        }
    }

    /// Reads one MIDI 2.0 channel voice message. Real-time safe.
    pub fn midi2(&mut self, offset: u32, message: ChannelVoice2, mut emit: impl FnMut(Event)) {
        let emit = &mut emit;
        let channel = message.channel() & 0x0F;
        let role = self.layout.role(channel);
        let member = matches!(role, Role::Member(_));
        match message {
            ChannelVoice2::NoteOn {
                note,
                velocity,
                attribute,
                ..
            } => {
                let pitch = attribute
                    .as_pitch()
                    .map(|_| u32::from(attribute.data()) << 16);
                let velocity = unit(u32::from(velocity), 16);
                self.start(offset, channel, note & 0x7F, velocity, pitch, None, emit);
            }
            ChannelVoice2::NoteOff { note, velocity, .. } => {
                let velocity = unit(u32::from(velocity), 16);
                if !self.end(offset, channel, note & 0x7F, velocity, emit) {
                    // A note this converter did not start, perhaps before
                    // the stream changed protocol: let channel and key
                    // find it.
                    let note = NoteRef::new(channel, note & 0x7F);
                    emit(Event {
                        offset,
                        kind: EventKind::NoteOff { note, velocity },
                    });
                }
            }
            ChannelVoice2::PolyPressure { note, pressure, .. } => {
                self.per_note(
                    offset,
                    channel,
                    note & 0x7F,
                    Dimension::Pressure,
                    pressure,
                    emit,
                );
            }
            ChannelVoice2::PerNotePitchBend { note, value, .. } => {
                self.per_note(offset, channel, note & 0x7F, Dimension::Bend, value, emit);
            }
            ChannelVoice2::RegisteredPerNoteController {
                note,
                controller,
                value,
                ..
            } => {
                let dimension = match controller {
                    per_note::PITCH_7_25 => Dimension::Pitch,
                    per_note::VOLUME => Dimension::Volume,
                    per_note::PAN => Dimension::Pan,
                    per_note::EXPRESSION => Dimension::Expression,
                    per_note::BRIGHTNESS => Dimension::Timbre,
                    per_note::VIBRATO_DEPTH => Dimension::Vibrato,
                    _ => return,
                };
                self.per_note(offset, channel, note & 0x7F, dimension, value, emit);
            }
            ChannelVoice2::PerNoteManagement {
                note,
                detach,
                reset,
                ..
            } => self.management(offset, channel, note & 0x7F, detach, reset, emit),
            ChannelVoice2::PitchBend { value, .. } if member => {
                self.member(offset, channel, Dimension::Bend, value, emit);
            }
            ChannelVoice2::ChannelPressure { pressure, .. } if member => {
                self.member(offset, channel, Dimension::Pressure, pressure, emit);
            }
            ChannelVoice2::ControlChange {
                controller: TIMBRE_CONTROLLER,
                value,
                ..
            } if member => self.member(offset, channel, Dimension::Timbre, value, emit),
            ChannelVoice2::ControlChange { controller, .. } if member && controller >= 120 => {}
            ChannelVoice2::RegisteredController {
                bank, index, value, ..
            } => {
                // The 32-bit value as the 14 bits of a MIDI 1.0 parameter
                // (M2-104-UM, figure 97), which is what the MPE parameters
                // are defined in.
                let event = ParameterEvent {
                    channel,
                    parameter: Parameter::registered(bank & 0x7F, index & 0x7F),
                    change: Change::Value {
                        value: (value >> 18) as u16,
                        fine: false,
                    },
                };
                self.parameter(offset, &event, emit);
                if (bank, index) == PER_NOTE_BEND_RANGE {
                    self.channels[usize::from(channel)].per_note_range = cents(event.change);
                    self.refresh(offset, |note| note.source == channel && !note.member, emit);
                }
                if !member {
                    pass_on(offset, message, emit);
                }
            }
            ChannelVoice2::ControlChange { controller, .. } => {
                match (role, controller) {
                    (_, ALL_SOUND_OFF | ALL_NOTES_OFF..=127) => self.forget(channel),
                    (Role::Master(kind), RESET_ALL_CONTROLLERS) => {
                        self.reset_zone(offset, kind, emit);
                    }
                    _ => {}
                }
                pass_on(offset, message, emit);
            }
            _ => pass_on(offset, message, emit),
        }
    }

    /// Reads one packet: MIDI 1.0 and MIDI 2.0 channel voice messages, of
    /// any group. Everything else, and a packet that is not a valid
    /// message, is ignored. Real-time safe.
    pub fn packet(&mut self, offset: u32, packet: &Packet, emit: impl FnMut(Event)) {
        match packet.message() {
            Ok(Message::Midi1 { message, .. }) => self.midi1(offset, message, emit),
            Ok(Message::Midi2 { message, .. }) => self.midi2(offset, message, emit),
            _ => {}
        }
    }

    /// Reads a note-on from a format that has notes of its own, such as
    /// VST 3. On a channel of an MPE zone the note is followed as one that
    /// came as MIDI, keeping the identifier the host gave it if it gave
    /// one, so that the host's own expressions still find it. Anywhere
    /// else it is passed on unchanged. Real-time safe.
    pub fn note_on(
        &mut self,
        offset: u32,
        note: NoteRef,
        velocity: f32,
        mut emit: impl FnMut(Event),
    ) {
        let channel = note.channel & 0x0F;
        if self.layout.role(channel) == Role::Outside {
            emit(Event {
                offset,
                kind: EventKind::NoteOn { note, velocity },
            });
        } else {
            self.start(
                offset,
                channel,
                note.key & 0x7F,
                velocity,
                None,
                note.id,
                &mut emit,
            );
        }
    }

    /// Reads a note-off from a format that has notes of its own. The
    /// counterpart of [`note_on`](Self::note_on). Real-time safe.
    pub fn note_off(
        &mut self,
        offset: u32,
        note: NoteRef,
        velocity: f32,
        mut emit: impl FnMut(Event),
    ) {
        let channel = note.channel & 0x0F;
        if !self.end(offset, channel, note.key & 0x7F, velocity, &mut emit) {
            emit(Event {
                offset,
                kind: EventKind::NoteOff { note, velocity },
            });
        }
    }

    /// The velocity of a MIDI 1.0 note, with the low bits that controller
    /// 88 sent before it, if it did.
    fn velocity(&mut self, channel: u8, velocity: u8) -> Velocity {
        Velocity {
            high: velocity & 0x7F,
            low: self.channels[usize::from(channel)].velocity_lsb.take(),
        }
    }

    fn midi1_note_off(
        &mut self,
        offset: u32,
        channel: u8,
        key: u8,
        velocity: u8,
        emit: &mut impl FnMut(Event),
    ) {
        let velocity = self.velocity(channel, velocity);
        if !self.end(offset, channel, key, velocity.wide(), emit) {
            plain_note(offset, channel, key, velocity, false, emit);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn midi1_controller(
        &mut self,
        offset: u32,
        message: MidiMessage,
        channel: u8,
        controller: u8,
        value: u8,
        role: Role,
        emit: &mut impl FnMut(Event),
    ) {
        let value = value & 0x7F;
        match role {
            Role::Member(_) => match controller {
                TIMBRE_CONTROLLER => {
                    self.member(
                        offset,
                        channel,
                        Dimension::Timbre,
                        scale_7_to_32(value),
                        emit,
                    );
                }
                VELOCITY_PREFIX => self.channels[usize::from(channel)].velocity_lsb = Some(value),
                // Zone messages: ignored on member channels (section 2.3.1).
                ALL_SOUND_OFF..=127 => {}
                _ => emit(Event::from_midi(offset, message)),
            },
            Role::Master(kind) => {
                match controller {
                    VELOCITY_PREFIX => {
                        self.channels[usize::from(channel)].velocity_lsb = Some(value);
                    }
                    ALL_SOUND_OFF | ALL_NOTES_OFF..=127 => self.forget(channel),
                    RESET_ALL_CONTROLLERS => self.reset_zone(offset, kind, emit),
                    _ => {}
                }
                emit(Event::from_midi(offset, message));
            }
            Role::Outside => {
                match controller {
                    VELOCITY_PREFIX => {
                        self.channels[usize::from(channel)].velocity_lsb = Some(value);
                    }
                    ALL_SOUND_OFF | ALL_NOTES_OFF..=127 => self.forget(channel),
                    _ => {}
                }
                emit(Event::from_midi(offset, message));
            }
        }
    }

    /// Acts on a registered parameter: the MPE configuration message, and
    /// the bend range of member channels.
    fn parameter(&mut self, offset: u32, event: &ParameterEvent, emit: &mut impl FnMut(Event)) {
        if let Some(configuration) = Configuration::from_event(event) {
            if self.mpe != Mpe::Off {
                let mut layout = self.layout;
                layout.configure(configuration);
                self.apply_layout(offset, layout, Some(configuration.zone), emit);
            }
            return;
        }
        if event.parameter != Parameter::PITCH_BEND_SENSITIVITY {
            return;
        }
        // Only a member channel's range is this converter's: the range of
        // a master or ordinary channel goes with its channel-wide bend.
        let Role::Member(kind) = self.layout.role(event.channel) else {
            return;
        };
        let range = match event.change {
            Change::Value { .. } => cents(event.change),
            Change::FineOnly(fine) => {
                let whole = self.channels[usize::from(event.channel)].member_range / 100;
                whole * 100 + u16::from(fine & 0x7F).min(99)
            }
            Change::Increment(_) | Change::Decrement(_) => return,
        };
        // Member channels of a zone share one range (section 2.4).
        for channel in 0..16u8 {
            if self.layout.role(channel) == Role::Member(kind) {
                self.channels[usize::from(channel)].member_range = range;
            }
        }
        let layout = self.layout;
        self.refresh(
            offset,
            |note| note.member && layout.role(note.source) == Role::Member(kind),
            emit,
        );
    }

    /// Moves to a new set of zones. `configured` is the zone a
    /// configuration message named, or `None` for both.
    fn apply_layout(
        &mut self,
        offset: u32,
        layout: ZoneLayout,
        configured: Option<ZoneKind>,
        emit: &mut impl FnMut(Event),
    ) {
        let old = self.layout;
        for channel in 0..16u8 {
            let role = layout.role(channel);
            if old.role(channel) != role {
                // Section 2.1.4: the notes of a channel that enters or
                // leaves a zone, or changes role in one, stop, and its
                // controls go back to rest.
                self.release_where(offset, |note| note.source == channel, emit);
                self.channels[usize::from(channel)] = Channel::NEW;
            } else if match (role, configured) {
                (_, None) => true,
                (Role::Master(kind) | Role::Member(kind), Some(zone)) => kind == zone,
                (Role::Outside, Some(_)) => false,
            } {
                // Section 2.4: configuring a zone puts its bend ranges
                // back to their defaults.
                self.channels[usize::from(channel)].member_range = DEFAULT_RANGE_CENTS;
            }
        }
        self.layout = layout;
        self.refresh(offset, |_| true, emit);
    }

    /// Reset all controllers on a master channel: the zone's member
    /// channels' pitch bend and pressure, and the master's polyphonic
    /// pressure, go back to rest. Controller 74 stays, as RP-015 says of
    /// the sound controllers.
    fn reset_zone(&mut self, offset: u32, kind: ZoneKind, emit: &mut impl FnMut(Event)) {
        let master = kind.master_channel();
        for channel in 0..16u8 {
            if self.layout.role(channel) == Role::Member(kind) {
                let state = &mut self.channels[usize::from(channel)];
                state.bend = CENTRE;
                state.pressure = 0;
            }
        }
        for key in 0..128 {
            self.keys[key_index(master, key)].pressure = 0;
        }
        let layout = self.layout;
        for note in &mut self.notes {
            if note.member && layout.role(note.source) == Role::Member(kind) {
                note.shape.bend = CENTRE;
                note.shape.pressure = 0;
            } else if note.source == master {
                note.shape.pressure = 0;
            }
        }
        self.refresh(offset, |note| note.reported == master, emit);
    }

    /// Starts a note that arrived on `channel`, which is not an ordinary
    /// MIDI 1.0 one, with a velocity from 0 to 1, a pitch if the message
    /// gave one, and the host's identifier if it is a native note.
    #[allow(clippy::too_many_arguments)]
    fn start(
        &mut self,
        offset: u32,
        channel: u8,
        key: u8,
        velocity: f32,
        pitch: Option<u32>,
        id: Option<u32>,
        emit: &mut impl FnMut(Event),
    ) {
        // The same number struck again on the same channel ends the note
        // it is attached to. A detached one plays on.
        if let Some(index) = self
            .notes
            .iter()
            .position(|note| note.source == channel && note.key == key && !note.detached)
        {
            let note = self.notes.remove(index);
            emit_off(offset, &note, 0.0, emit);
        }
        if self.notes.len() >= self.capacity {
            let oldest = self.notes.remove(0);
            emit_off(offset, &oldest, 0.0, emit);
        }
        let (reported, member) = match self.layout.role(channel) {
            Role::Member(kind) => (kind.master_channel(), true),
            _ => (channel, false),
        };
        let mut shape = self.keys[key_index(channel, key)];
        if member {
            let own = self.channels[usize::from(channel)];
            shape.bend = own.bend;
            shape.pressure = own.pressure;
            shape.timbre = own.timbre;
        }
        if pitch.is_some() {
            shape.pitch = pitch;
        }
        let id = id.unwrap_or_else(|| {
            let id = 0x8000_0000 | self.next_id;
            self.next_id = self.next_id.wrapping_add(1) & 0x7FFF_FFFF;
            id
        });
        let mut note = Note {
            id,
            source: channel,
            key,
            reported,
            member,
            detached: false,
            shape,
            sent: Sent(EXPRESSIONS.map(|(_, rest)| rest)),
        };
        emit(Event {
            offset,
            kind: EventKind::NoteOn {
                note: note.reference(),
                velocity,
            },
        });
        let ranges = self.ranges(&note);
        update(offset, &mut note, ranges, emit);
        self.notes.push(note);
    }

    /// Ends the oldest note held on `key` of `channel`. Returns whether
    /// there was one.
    fn end(
        &mut self,
        offset: u32,
        channel: u8,
        key: u8,
        velocity: f32,
        emit: &mut impl FnMut(Event),
    ) -> bool {
        let Some(index) = self
            .notes
            .iter()
            .position(|note| note.source == channel && note.key == key)
        else {
            return false;
        };
        let note = self.notes.remove(index);
        emit_off(offset, &note, velocity, emit);
        true
    }

    /// Releases every note `releases` picks.
    fn release_where(
        &mut self,
        offset: u32,
        releases: impl Fn(&Note) -> bool,
        emit: &mut impl FnMut(Event),
    ) {
        self.notes.retain(|note| {
            if releases(note) {
                emit_off(offset, note, 0.0, emit);
                false
            } else {
                true
            }
        });
    }

    /// Forgets the notes reported on `channel`, which a channel message
    /// passed on ends for the processor.
    fn forget(&mut self, channel: u8) {
        self.notes.retain(|note| note.reported != channel);
    }

    /// A dimension of a member channel moved.
    fn member(
        &mut self,
        offset: u32,
        channel: u8,
        dimension: Dimension,
        value: u32,
        emit: &mut impl FnMut(Event),
    ) {
        let state = &mut self.channels[usize::from(channel)];
        match dimension {
            Dimension::Bend => state.bend = value,
            Dimension::Pressure => state.pressure = value,
            _ => state.timbre = value,
        }
        let ranges = self.ranges_of(channel);
        for note in &mut self.notes {
            if note.member && note.source == channel {
                set(&mut note.shape, dimension, value);
                update(offset, note, ranges, emit);
            }
        }
    }

    /// A per-note value of `key` on `channel` moved.
    fn per_note(
        &mut self,
        offset: u32,
        channel: u8,
        key: u8,
        dimension: Dimension,
        value: u32,
        emit: &mut impl FnMut(Event),
    ) {
        set(&mut self.keys[key_index(channel, key)], dimension, value);
        let ranges = self.ranges_of(channel);
        for note in &mut self.notes {
            if note.source == channel && note.key == key && !note.detached {
                set(&mut note.shape, dimension, value);
                update(offset, note, ranges, emit);
            }
        }
    }

    /// Per-note management (M2-104-UM, section 7.4.5).
    fn management(
        &mut self,
        offset: u32,
        channel: u8,
        key: u8,
        detach: bool,
        reset: bool,
        emit: &mut impl FnMut(Event),
    ) {
        let attached = |note: &Note| note.source == channel && note.key == key && !note.detached;
        if detach {
            for note in self.notes.iter_mut().filter(|note| attached(note)) {
                note.detached = true;
            }
        }
        if reset {
            self.keys[key_index(channel, key)] = Shape::REST;
            let ranges = self.ranges_of(channel);
            let own = self.channels[usize::from(channel)];
            for note in self.notes.iter_mut().filter(|note| attached(note)) {
                note.shape = Shape::REST;
                if note.member {
                    // A member channel's own dimensions are the channel's,
                    // not the number's.
                    note.shape.bend = own.bend;
                    note.shape.pressure = own.pressure;
                    note.shape.timbre = own.timbre;
                }
                update(offset, note, ranges, emit);
            }
        }
    }

    /// The bend ranges, in cents, that apply to notes arriving on
    /// `channel`: as a member channel's, and as per-note bend.
    fn ranges_of(&self, channel: u8) -> (u16, u16) {
        let state = self.channels[usize::from(channel)];
        (state.member_range, state.per_note_range)
    }

    fn ranges(&self, note: &Note) -> (u16, u16) {
        self.ranges_of(note.source)
    }

    /// Sends whatever changed in the notes `which` picks, after a range or
    /// the zones changed.
    fn refresh(
        &mut self,
        offset: u32,
        which: impl Fn(&Note) -> bool,
        emit: &mut impl FnMut(Event),
    ) {
        let channels = self.channels;
        for note in &mut self.notes {
            if which(note) {
                let state = channels[usize::from(note.source)];
                update(
                    offset,
                    note,
                    (state.member_range, state.per_note_range),
                    emit,
                );
            }
        }
    }
}

impl Default for NoteInput {
    fn default() -> Self {
        Self::new(Mpe::Follow)
    }
}

fn key_index(channel: u8, key: u8) -> usize {
    usize::from(channel & 0x0F) * 128 + usize::from(key & 0x7F)
}

fn set(shape: &mut Shape, dimension: Dimension, value: u32) {
    match dimension {
        Dimension::Bend => shape.bend = value,
        Dimension::Pressure => shape.pressure = value,
        Dimension::Timbre => shape.timbre = value,
        Dimension::Volume => shape.volume = value,
        Dimension::Expression => shape.expression = value,
        Dimension::Pan => shape.pan = value,
        Dimension::Vibrato => shape.vibrato = value,
        Dimension::Pitch => shape.pitch = Some(value),
    }
}

/// A bend range in cents from a parameter's 14-bit value: semitones in
/// the coarse byte, cents (up to 99) in the fine one.
fn cents(change: Change) -> u16 {
    match change {
        Change::Value { value, .. } => (value >> 7) * 100 + (value & 0x7F).min(99),
        _ => DEFAULT_RANGE_CENTS,
    }
}

/// A `bits`-wide value as a fraction of its maximum, from 0 to 1.
fn unit(value: u32, bits: u32) -> f32 {
    let max = if bits >= 32 {
        u32::MAX
    } else {
        (1 << bits) - 1
    };
    (f64::from(value.min(max)) / f64::from(max)) as f32
}

fn unit_32(value: u32) -> f64 {
    f64::from(value) / f64::from(u32::MAX)
}

/// A 32-bit value whose centre is its rest, from 0 to 1 with the centre
/// exactly one half: controller 74 at 64 is brightness 0.5.
fn centred_unit(value: u32) -> f64 {
    0.5 + 0.5 * bipolar(value)
}

/// A 32-bit value with its centre as zero, from -1 to 1: the minimum is
/// -1, the centre 0 and the maximum exactly 1, as the min-centre-max
/// scaling of the specification (appendix D.1.3) places them.
fn bipolar(value: u32) -> f64 {
    let from_centre = f64::from(value) - f64::from(CENTRE);
    if value >= CENTRE {
        from_centre / f64::from(u32::MAX - CENTRE)
    } else {
        from_centre / f64::from(CENTRE)
    }
}

/// The expressions of a note, in the order of [`EXPRESSIONS`].
fn expressions(note: &Note, (member_range, per_note_range): (u16, u16)) -> [f64; 6] {
    let shape = &note.shape;
    let range = if note.member {
        member_range
    } else {
        per_note_range
    };
    let mut tuning = bipolar(shape.bend) * f64::from(range) / 100.0;
    if let Some(pitch) = shape.pitch {
        // 7.25 fixed point: seven bits of semitones, 25 of fraction.
        tuning += f64::from(pitch) / f64::from(1u32 << 25) - f64::from(note.key);
    }
    let gain = |value: u32| unit_32(value) * unit_32(value);
    [
        tuning,
        unit_32(shape.pressure),
        centred_unit(shape.timbre),
        gain(shape.volume) * gain(shape.expression),
        bipolar(shape.pan),
        centred_unit(shape.vibrato),
    ]
}

/// Sends the expressions of `note` that differ from what it was last sent.
fn update(offset: u32, note: &mut Note, ranges: (u16, u16), emit: &mut impl FnMut(Event)) {
    let values = expressions(note, ranges);
    for (index, &(expression, _)) in EXPRESSIONS.iter().enumerate() {
        let value = values[index];
        if value.to_bits() != note.sent.0[index].to_bits() {
            note.sent.0[index] = value;
            emit(Event {
                offset,
                kind: EventKind::NoteExpression {
                    note: note.reference(),
                    expression,
                    value,
                },
            });
        }
    }
}

fn emit_off(offset: u32, note: &Note, velocity: f32, emit: &mut impl FnMut(Event)) {
    emit(Event {
        offset,
        kind: EventKind::NoteOff {
            note: note.reference(),
            velocity,
        },
    });
}

/// The velocity of a MIDI 1.0 note: seven bits, and seven more when a
/// high-resolution velocity prefix came before it.
#[derive(Debug, Clone, Copy)]
struct Velocity {
    high: u8,
    low: Option<u8>,
}

impl Velocity {
    /// From 0 to 1 as plain MIDI reads it: as [`Event::from_midi`] does
    /// without a prefix, so that plain MIDI sounds as it always has, and
    /// over fourteen bits with one.
    fn plain(self) -> f32 {
        match self.low {
            None => f32::from(self.high) / 127.0,
            Some(low) => f32::from((u16::from(self.high) << 7) | u16::from(low)) / 16_383.0,
        }
    }

    /// From 0 to 1 by way of MIDI 2.0's sixteen bits, widened as the
    /// specification's default translation widens it, so that the same
    /// note sent over MIDI 2.0 gets the same velocity.
    fn wide(self) -> f32 {
        let wide = match self.low {
            None => scale_up(u32::from(self.high), 7, 16),
            Some(low) => scale_up((u32::from(self.high) << 7) | u32::from(low), 14, 16),
        };
        unit(wide, 16)
    }
}

/// A note-on or note-off of plain MIDI 1.0.
fn plain_note(
    offset: u32,
    channel: u8,
    key: u8,
    velocity: Velocity,
    on: bool,
    emit: &mut impl FnMut(Event),
) {
    let velocity = velocity.plain();
    let note = NoteRef::new(channel, key);
    let kind = if on {
        EventKind::NoteOn { note, velocity }
    } else {
        EventKind::NoteOff { note, velocity }
    };
    emit(Event { offset, kind });
}

/// Passes on a channel-wide MIDI 2.0 message as the MIDI 1.0 messages the
/// default translation gives.
fn pass_on(offset: u32, message: ChannelVoice2, emit: &mut impl FnMut(Event)) {
    for &translated in midi2_to_midi1(message).as_slice() {
        emit(Event::from_midi(offset, translated));
    }
}
