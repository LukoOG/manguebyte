//! MIDI 2.0 per-note messages written out as MPE.

use crate::mpe::{ChannelAllocator, MEMBER_PITCH_BEND_RANGE, TIMBRE_CONTROLLER, Zone};
use crate::ump::{ChannelVoice2, PITCH_BEND_CENTRE};
use crate::{MidiMessage, translate};

/// Registered per-note controllers this translator has an MPE form for
/// (M2-104-UM, section 7.4.4).
const PER_NOTE_PITCH_7_25: u8 = 3;
/// Registered controller 0/7: the range of per-note pitch bend.
const PER_NOTE_BEND_RANGE: (u8, u8) = (0, 7);
/// Default range of per-note pitch bend and of MPE member channels, in
/// cents.
const DEFAULT_RANGE_CENTS: u16 = MEMBER_PITCH_BEND_RANGE as u16 * 100;
const ALL_SOUND_OFF: u8 = 120;
const ALL_NOTES_OFF: u8 = 123;

/// What one note number of one source channel holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Key {
    /// Per-note pitch bend, [`PITCH_BEND_CENTRE`] for none.
    bend: u32,
    pressure: u32,
    /// Per-note controller 74, the centre at rest.
    timbre: u32,
    /// Per-note controller 3: the number's pitch in 7.25 fixed point.
    pitch: Option<u32>,
    /// The member channel of the note playing on this number and still
    /// following its per-note messages.
    member: Option<u8>,
    /// The pitch of that note, from its pitch attribute or controller 3.
    note_pitch: Option<u32>,
    /// One bit per member channel on which a note of this number plays
    /// that per-note management has detached.
    detached: u16,
}

impl Key {
    const REST: Self = Self {
        bend: PITCH_BEND_CENTRE,
        pressure: 0,
        timbre: PITCH_BEND_CENTRE,
        pitch: None,
        member: None,
        note_pitch: None,
        detached: 0,
    };
}

/// Writes MIDI 2.0 channel voice messages out as MIDI Polyphonic
/// Expression, for a receiver that speaks MIDI 1.0.
///
/// The default translation ([`midi2_to_midi1`](super::midi2_to_midi1))
/// drops everything that shapes a single note, since MIDI 1.0 has no
/// message for it (M2-104-UM, section D.2.8). MPE has a way: each note
/// gets a member channel of its own, whose pitch bend, channel pressure
/// and controller 74 then belong to that note alone. This is that
/// alternate translation, following the sender's rules of "MIDI
/// Polyphonic Expression" 1.0 (RP-053):
///
/// - The first message is preceded by the zone's configuration message
///   (section 2.1.1), so a receiver that follows the stream knows the
///   zone. [`reset`](Self::reset) makes it go out again.
/// - A note-on gets a member channel from a [`ChannelAllocator`] (section
///   3.2), and the note's pitch bend, pressure and timbre are sent on that
///   channel before the note-on, so that the note starts with them
///   (section 3.3.1). The note-on and its note-off go on that channel.
/// - Per-note pitch bend, the pitch attribute of a note-on (type 3) and
///   registered per-note controller 3 (pitch, 7.25) become the member
///   channel's pitch bend, scaled from the per-note bend range (48
///   semitones until registered controller 0/7 says otherwise) to the
///   member channels' 48 semitones, and clamped to it. Polyphonic pressure
///   becomes the member channel's channel pressure, and registered
///   per-note controller 74 its controller 74.
/// - Per-note values belong to the note number, as in MIDI 2.0: a value
///   sent before a note-on is that note's starting value. Per-note
///   management detaches the playing notes of a number from later
///   per-note messages, or puts the number's values back to rest.
/// - Everything that applies to the whole channel is translated by the
///   default translation and sent on the zone's master channel, whatever
///   channel it came on, so that it reaches every note of the zone. "All
///   notes off", "all sound off" and the mode messages also make the
///   translator forget its notes.
///
/// Values are cut to MIDI 1.0's resolution as the default translation
/// cuts them (section D.1.4): velocity and pressure to 7 bits, pitch bend
/// to 14. A note-on whose velocity would become zero gets 1 (section
/// D.2.1).
///
/// After [`new`](Self::new), which allocates, every method is real-time
/// safe.
///
/// ```
/// use manguebyte_midi::mpe::Zone;
/// use manguebyte_midi::translate::Midi2ToMpe;
/// use manguebyte_midi::ump::{ChannelVoice2, NoteAttribute, PITCH_BEND_CENTRE};
/// use manguebyte_midi::MidiMessage;
///
/// let mut mpe = Midi2ToMpe::new(Zone::lower(15).unwrap());
/// let mut sent = Vec::new();
/// // Middle C bent up by a quarter of the per-note range, 12 semitones.
/// mpe.translate(
///     ChannelVoice2::PerNotePitchBend { channel: 0, note: 60, value: PITCH_BEND_CENTRE + (1 << 29) },
///     |message| sent.push(message),
/// );
/// sent.clear(); // the configuration message
/// mpe.translate(
///     ChannelVoice2::NoteOn { channel: 0, note: 60, velocity: 0xC000, attribute: NoteAttribute::NONE },
///     |message| sent.push(message),
/// );
/// assert_eq!(sent, [
///     MidiMessage::PitchBend { channel: 1, value: 2048 },
///     MidiMessage::ChannelPressure { channel: 1, pressure: 0 },
///     MidiMessage::ControlChange { channel: 1, controller: 74, value: 64 },
///     MidiMessage::NoteOn { channel: 1, note: 60, velocity: 96 },
/// ]);
/// ```
///
/// # Limits
///
/// MPE carries three dimensions. Registered per-note controllers other
/// than 3 and 74 (volume, pan, vibrato and the rest), assignable per-note
/// controllers and relative controllers have no MPE form and are dropped,
/// and so is registered controller 0/7 once it has been read. A note
/// attribute other than pitch is dropped. When the zone has fewer member
/// channels than there are notes, notes share a channel, as section 3.2
/// prefers to refusing them, and then follow each other's bend, pressure
/// and timbre. Every source channel's notes go into the one zone. A
/// pitch beyond the member range of ±48 semitones from the note number is
/// clamped to it. Velocity goes out with 7 bits: no high-resolution
/// velocity prefix is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Midi2ToMpe {
    zone: Zone,
    allocator: ChannelAllocator,
    /// One entry per source channel and note number.
    keys: Box<[Key]>,
    /// The per-note bend range of each source channel, in cents.
    ranges: [u16; 16],
    /// Whether the configuration message has gone out.
    configured: bool,
}

impl Midi2ToMpe {
    /// A translator that plays into `zone`. Allocates.
    pub fn new(zone: Zone) -> Self {
        Self {
            zone,
            allocator: ChannelAllocator::new(zone),
            keys: vec![Key::REST; 16 * 128].into_boxed_slice(),
            ranges: [DEFAULT_RANGE_CENTS; 16],
            configured: false,
        }
    }

    /// The zone notes are played into.
    pub fn zone(&self) -> Zone {
        self.zone
    }

    /// Forgets every note and per-note value, as at the start, so that
    /// the next message is preceded by the configuration message again.
    /// Nothing is sent: a sender that stops in the middle of notes ends
    /// them first, with [`end_all`](Self::end_all).
    pub fn reset(&mut self) {
        self.allocator.reset();
        self.keys.fill(Key::REST);
        self.ranges = [DEFAULT_RANGE_CENTS; 16];
        self.configured = false;
    }

    /// Sends a note-off for every note still playing, and forgets them.
    pub fn end_all(&mut self, mut emit: impl FnMut(MidiMessage)) {
        for (index, key) in self.keys.iter_mut().enumerate() {
            let note = (index % 128) as u8;
            let mut channels = key.detached;
            if let Some(member) = key.member {
                channels |= 1 << member;
            }
            for channel in 0..16u8 {
                if channels & (1 << channel) != 0 {
                    emit(MidiMessage::NoteOff {
                        channel,
                        note,
                        velocity: 0,
                    });
                }
            }
            key.member = None;
            key.note_pitch = None;
            key.detached = 0;
        }
        self.allocator.reset();
    }

    /// Translates one message, handing each MIDI 1.0 message it becomes to
    /// `emit` in the order they are to be sent.
    pub fn translate(&mut self, message: ChannelVoice2, mut emit: impl FnMut(MidiMessage)) {
        let emit = &mut emit;
        if !self.configured {
            self.configured = true;
            for &configuration in self.zone.configuration().messages().as_slice() {
                emit(configuration);
            }
        }
        let source = message.channel() & 0x0F;
        match message {
            ChannelVoice2::NoteOn {
                note,
                velocity,
                attribute,
                ..
            } => {
                let note = note & 0x7F;
                // The number struck again ends the note attached to it, as
                // a MIDI 2.0 receiver does.
                self.end_attached(source, note, 0, emit);
                let member = self.allocator.note_on(note);
                let range = self.ranges[usize::from(source)];
                let key = &mut self.keys[index(source, note)];
                key.member = Some(member);
                key.note_pitch = attribute
                    .as_pitch()
                    .map(|_| u32::from(attribute.data()) << 16)
                    .or(key.pitch);
                let key = *key;
                emit(bend_message(member, note, &key, range));
                emit(MidiMessage::ChannelPressure {
                    channel: member,
                    pressure: translate::scale_32_to_7(key.pressure),
                });
                emit(MidiMessage::ControlChange {
                    channel: member,
                    controller: TIMBRE_CONTROLLER,
                    value: translate::scale_32_to_7(key.timbre),
                });
                emit(MidiMessage::NoteOn {
                    channel: member,
                    note,
                    velocity: translate::scale_16_to_7(velocity).max(1),
                });
            }
            ChannelVoice2::NoteOff { note, velocity, .. } => {
                let note = note & 0x7F;
                let velocity = translate::scale_16_to_7(velocity);
                // The oldest note of the number ends first: a detached one
                // before the one still attached.
                let key = &mut self.keys[index(source, note)];
                if key.detached != 0 {
                    let channel = key.detached.trailing_zeros() as u8;
                    key.detached &= !(1 << channel);
                    self.allocator.note_off(channel, note);
                    emit(MidiMessage::NoteOff {
                        channel,
                        note,
                        velocity,
                    });
                } else {
                    self.end_attached(source, note, velocity, emit);
                }
            }
            ChannelVoice2::PolyPressure { note, pressure, .. } => {
                let key = &mut self.keys[index(source, note & 0x7F)];
                key.pressure = pressure;
                if let Some(channel) = key.member {
                    emit(MidiMessage::ChannelPressure {
                        channel,
                        pressure: translate::scale_32_to_7(pressure),
                    });
                }
            }
            ChannelVoice2::PerNotePitchBend { note, value, .. } => {
                let note = note & 0x7F;
                self.keys[index(source, note)].bend = value;
                self.send_bend(source, note, emit);
            }
            ChannelVoice2::RegisteredPerNoteController {
                note,
                controller,
                value,
                ..
            } => {
                let note = note & 0x7F;
                match controller {
                    TIMBRE_CONTROLLER => {
                        let key = &mut self.keys[index(source, note)];
                        key.timbre = value;
                        if let Some(channel) = key.member {
                            emit(MidiMessage::ControlChange {
                                channel,
                                controller: TIMBRE_CONTROLLER,
                                value: translate::scale_32_to_7(value),
                            });
                        }
                    }
                    PER_NOTE_PITCH_7_25 => {
                        let key = &mut self.keys[index(source, note)];
                        key.pitch = Some(value);
                        if key.member.is_some() {
                            key.note_pitch = Some(value);
                        }
                        self.send_bend(source, note, emit);
                    }
                    _ => {}
                }
            }
            ChannelVoice2::PerNoteManagement {
                note,
                detach,
                reset,
                ..
            } => {
                let note = note & 0x7F;
                let key = &mut self.keys[index(source, note)];
                if detach {
                    if let Some(member) = key.member.take() {
                        key.detached |= 1 << member;
                    }
                    key.note_pitch = None;
                }
                if reset {
                    let kept = (key.member, key.detached);
                    *key = Key::REST;
                    (key.member, key.detached) = kept;
                    if let Some(channel) = key.member {
                        let range = self.ranges[usize::from(source)];
                        let key = *key;
                        emit(bend_message(channel, note, &key, range));
                        emit(MidiMessage::ChannelPressure {
                            channel,
                            pressure: 0,
                        });
                        emit(MidiMessage::ControlChange {
                            channel,
                            controller: TIMBRE_CONTROLLER,
                            value: translate::scale_32_to_7(key.timbre),
                        });
                    }
                }
            }
            ChannelVoice2::RegisteredController {
                bank, index, value, ..
            } if (bank & 0x7F, index & 0x7F) == PER_NOTE_BEND_RANGE => {
                // Read, not passed on: MIDI 1.0 has no per-note bend, so
                // the range is this translator's to apply.
                let semitones = (value >> 25) as u16;
                let cents = ((value >> 18) & 0x7F) as u16;
                self.ranges[usize::from(source)] = semitones * 100 + cents.min(99);
                for note in 0..128 {
                    self.send_bend(source, note, emit);
                }
            }
            ChannelVoice2::ControlChange { controller, .. } => {
                let master = self.zone.master_channel();
                for &translated in translate::midi2_to_midi1(message).as_slice() {
                    emit(on_channel(translated, master));
                }
                if matches!(controller & 0x7F, ALL_SOUND_OFF | ALL_NOTES_OFF..=127) {
                    self.forget();
                }
            }
            _ => {
                let master = self.zone.master_channel();
                for &translated in translate::midi2_to_midi1(message).as_slice() {
                    emit(on_channel(translated, master));
                }
            }
        }
    }

    /// Ends the note attached to `note` of `source`, if one is.
    fn end_attached(
        &mut self,
        source: u8,
        note: u8,
        velocity: u8,
        emit: &mut impl FnMut(MidiMessage),
    ) {
        let key = &mut self.keys[index(source, note)];
        if let Some(channel) = key.member.take() {
            key.note_pitch = None;
            self.allocator.note_off(channel, note);
            emit(MidiMessage::NoteOff {
                channel,
                note,
                velocity,
            });
        }
    }

    /// Sends the pitch bend of the note attached to `note` of `source`,
    /// if one is.
    fn send_bend(&self, source: u8, note: u8, emit: &mut impl FnMut(MidiMessage)) {
        let key = &self.keys[index(source, note)];
        if let Some(channel) = key.member {
            emit(bend_message(
                channel,
                note,
                key,
                self.ranges[usize::from(source)],
            ));
        }
    }

    /// Forgets every note, after a message that ends them all.
    fn forget(&mut self) {
        for key in self.keys.iter_mut() {
            key.member = None;
            key.note_pitch = None;
            key.detached = 0;
        }
        self.allocator.reset();
    }
}

fn index(channel: u8, note: u8) -> usize {
    usize::from(channel & 0x0F) * 128 + usize::from(note & 0x7F)
}

/// `message` on `channel` instead of its own.
fn on_channel(message: MidiMessage, channel: u8) -> MidiMessage {
    match message {
        MidiMessage::NoteOff { note, velocity, .. } => MidiMessage::NoteOff {
            channel,
            note,
            velocity,
        },
        MidiMessage::NoteOn { note, velocity, .. } => MidiMessage::NoteOn {
            channel,
            note,
            velocity,
        },
        MidiMessage::PolyPressure { note, pressure, .. } => MidiMessage::PolyPressure {
            channel,
            note,
            pressure,
        },
        MidiMessage::ControlChange {
            controller, value, ..
        } => MidiMessage::ControlChange {
            channel,
            controller,
            value,
        },
        MidiMessage::ProgramChange { program, .. } => {
            MidiMessage::ProgramChange { channel, program }
        }
        MidiMessage::ChannelPressure { pressure, .. } => {
            MidiMessage::ChannelPressure { channel, pressure }
        }
        MidiMessage::PitchBend { value, .. } => MidiMessage::PitchBend { channel, value },
    }
}

/// The member channel's pitch bend for a note on `note` shaped by `key`,
/// whose per-note bend range is `range` cents.
fn bend_message(channel: u8, note: u8, key: &Key, range: u16) -> MidiMessage {
    let bend14 = if range == DEFAULT_RANGE_CENTS && key.note_pitch.is_none() {
        // The same range on both sides: the value passes through, cut to
        // 14 bits as the default translation cuts pitch bend.
        translate::scale_32_to_14(key.bend)
    } else {
        let mut semitones = bipolar(key.bend) * f64::from(range) / 100.0;
        if let Some(pitch) = key.note_pitch {
            semitones += f64::from(pitch) / f64::from(1u32 << 25) - f64::from(note);
        }
        let position = (semitones / f64::from(MEMBER_PITCH_BEND_RANGE)).clamp(-1.0, 1.0);
        // The inverse of how a receiver widens and reads it: the centre is
        // 0x2000, the minimum -1 and the maximum exactly 1.
        let steps = if position >= 0.0 {
            position * f64::from(0x3FFF - 0x2000)
        } else {
            position * f64::from(0x2000)
        };
        (f64::from(0x2000) + steps.round()) as u16
    };
    MidiMessage::PitchBend {
        channel,
        value: bend14 as i16 - 0x2000,
    }
}

/// A 32-bit value with its centre as zero, from -1 to 1.
fn bipolar(value: u32) -> f64 {
    let from_centre = f64::from(value) - f64::from(PITCH_BEND_CENTRE);
    if value >= PITCH_BEND_CENTRE {
        from_centre / f64::from(u32::MAX - PITCH_BEND_CENTRE)
    } else {
        from_centre / f64::from(PITCH_BEND_CENTRE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mpe::{NoteEvent, NoteTracker, ZoneKind};
    use crate::translate::scale_7_to_32;
    use crate::ump::NoteAttribute;

    fn run(mpe: &mut Midi2ToMpe, message: ChannelVoice2) -> Vec<MidiMessage> {
        let mut sent = Vec::new();
        mpe.translate(message, |message| sent.push(message));
        sent
    }

    fn on(note: u8, velocity: u16) -> ChannelVoice2 {
        ChannelVoice2::NoteOn {
            channel: 0,
            note,
            velocity,
            attribute: NoteAttribute::NONE,
        }
    }

    fn off(note: u8) -> ChannelVoice2 {
        ChannelVoice2::NoteOff {
            channel: 0,
            note,
            velocity: 0x8000,
            attribute: NoteAttribute::NONE,
        }
    }

    fn translator() -> Midi2ToMpe {
        let mut mpe = Midi2ToMpe::new(Zone::lower(3).unwrap());
        let configuration = run(
            &mut mpe,
            ChannelVoice2::ChannelPressure {
                channel: 0,
                pressure: 0,
            },
        );
        // RP-053 section 2.1.1: registered parameter 6 on the master
        // channel, with the member count as its coarse byte.
        let bytes: Vec<u8> = configuration
            .iter()
            .flat_map(|message| message.to_bytes().as_slice().to_vec())
            .collect();
        assert_eq!(
            bytes,
            [
                0xB0, 0x64, 0x06, 0xB0, 0x65, 0x00, 0xB0, 0x06, 0x03, 0xD0, 0x00
            ]
        );
        mpe
    }

    #[test]
    fn each_note_gets_a_member_channel_with_its_own_dimensions() {
        let mut mpe = translator();
        let a = run(&mut mpe, on(60, 0xFFFF));
        let b = run(&mut mpe, on(64, 0x0001));
        assert_eq!(
            a.last(),
            Some(&MidiMessage::NoteOn {
                channel: 1,
                note: 60,
                velocity: 127,
            })
        );
        // Velocity that would round to zero is sent as 1 (D.2.1).
        assert_eq!(
            b.last(),
            Some(&MidiMessage::NoteOn {
                channel: 2,
                note: 64,
                velocity: 1,
            })
        );
        // Pressure and timbre of note 64 go to its channel alone.
        assert_eq!(
            run(
                &mut mpe,
                ChannelVoice2::PolyPressure {
                    channel: 0,
                    note: 64,
                    pressure: u32::MAX,
                }
            ),
            [MidiMessage::ChannelPressure {
                channel: 2,
                pressure: 127,
            }]
        );
        assert_eq!(
            run(
                &mut mpe,
                ChannelVoice2::RegisteredPerNoteController {
                    channel: 0,
                    note: 60,
                    controller: 74,
                    value: 0,
                }
            ),
            [MidiMessage::ControlChange {
                channel: 1,
                controller: 74,
                value: 0,
            }]
        );
        // The bend passes through at 14 bits when the ranges agree.
        assert_eq!(
            run(
                &mut mpe,
                ChannelVoice2::PerNotePitchBend {
                    channel: 0,
                    note: 60,
                    value: 0,
                }
            ),
            [MidiMessage::PitchBend {
                channel: 1,
                value: -8192,
            }]
        );
        assert_eq!(
            run(&mut mpe, off(60)),
            [MidiMessage::NoteOff {
                channel: 1,
                note: 60,
                velocity: 64,
            }]
        );
        // A message for a number with no note goes nowhere.
        assert!(
            run(
                &mut mpe,
                ChannelVoice2::PolyPressure {
                    channel: 0,
                    note: 60,
                    pressure: 1,
                }
            )
            .is_empty()
        );
    }

    #[test]
    fn channel_messages_go_to_the_master_channel() {
        let mut mpe = translator();
        assert_eq!(
            run(
                &mut mpe,
                ChannelVoice2::PitchBend {
                    channel: 5,
                    value: PITCH_BEND_CENTRE,
                }
            ),
            [MidiMessage::PitchBend {
                channel: 0,
                value: 0,
            }]
        );
        assert_eq!(
            run(
                &mut mpe,
                ChannelVoice2::ControlChange {
                    channel: 3,
                    controller: 64,
                    value: u32::MAX,
                }
            ),
            [MidiMessage::ControlChange {
                channel: 0,
                controller: 64,
                value: 127,
            }]
        );
        // Per-note controllers with no MPE form are dropped (D.2.8).
        assert!(
            run(
                &mut mpe,
                ChannelVoice2::AssignablePerNoteController {
                    channel: 0,
                    note: 60,
                    controller: 1,
                    value: 1,
                }
            )
            .is_empty()
        );
    }

    #[test]
    fn notes_start_with_the_values_sent_before_them_and_a_pitch_attribute() {
        let mut mpe = translator();
        run(
            &mut mpe,
            ChannelVoice2::PolyPressure {
                channel: 0,
                note: 50,
                pressure: scale_7_to_32(100),
            },
        );
        let sent = run(
            &mut mpe,
            ChannelVoice2::NoteOn {
                channel: 0,
                note: 50,
                velocity: 0x8000,
                // A quarter tone above the key.
                attribute: NoteAttribute::pitch(50.5),
            },
        );
        // Half a semitone of 48 is 1/96 of the way up: 85 steps of 8191.
        assert_eq!(
            sent,
            [
                MidiMessage::PitchBend {
                    channel: 1,
                    value: 85,
                },
                MidiMessage::ChannelPressure {
                    channel: 1,
                    pressure: 100,
                },
                MidiMessage::ControlChange {
                    channel: 1,
                    controller: 74,
                    value: 64,
                },
                MidiMessage::NoteOn {
                    channel: 1,
                    note: 50,
                    velocity: 64,
                },
            ]
        );
        // The attribute belonged to that note: the next one on the number
        // starts unbent, but keeps the number's pressure.
        run(&mut mpe, off(50));
        let again = run(&mut mpe, on(50, 0x8000));
        assert_eq!(
            again[0],
            MidiMessage::PitchBend {
                channel: 1,
                value: 0,
            }
        );
        assert_eq!(
            again[1],
            MidiMessage::ChannelPressure {
                channel: 1,
                pressure: 100,
            }
        );
    }

    #[test]
    fn a_wider_per_note_range_is_scaled_to_the_member_range_and_clamped() {
        let mut mpe = translator();
        // 96 semitones per-note range: registered controller 0/7.
        run(
            &mut mpe,
            ChannelVoice2::RegisteredController {
                channel: 0,
                bank: 0,
                index: 7,
                value: 96 << 25,
            },
        );
        run(&mut mpe, on(60, 0x8000));
        // A quarter of 96 is 24 semitones: half of the member range.
        let bend = |value| {
            let mut mpe = mpe.clone();
            run(
                &mut mpe,
                ChannelVoice2::PerNotePitchBend {
                    channel: 0,
                    note: 60,
                    value,
                },
            )
        };
        assert_eq!(
            bend(PITCH_BEND_CENTRE + (1 << 29)),
            [MidiMessage::PitchBend {
                channel: 1,
                value: 4096,
            }]
        );
        // All the way up is 96 semitones, past the 48 a member reaches.
        assert_eq!(
            bend(u32::MAX),
            [MidiMessage::PitchBend {
                channel: 1,
                value: 8191,
            }]
        );
        assert_eq!(
            bend(0),
            [MidiMessage::PitchBend {
                channel: 1,
                value: -8192,
            }]
        );
    }

    #[test]
    fn management_detaches_and_resets_and_a_repeated_number_ends_the_old_note() {
        let mut mpe = translator();
        run(&mut mpe, on(60, 0x8000));
        // Struck again: the first note ends before the second starts.
        let again = run(&mut mpe, on(60, 0x8000));
        assert_eq!(
            again[0],
            MidiMessage::NoteOff {
                channel: 1,
                note: 60,
                velocity: 0,
            }
        );
        assert_eq!(
            again.last(),
            Some(&MidiMessage::NoteOn {
                channel: 1,
                note: 60,
                velocity: 64,
            })
        );
        // Detached, it no longer follows the number.
        run(
            &mut mpe,
            ChannelVoice2::PerNoteManagement {
                channel: 0,
                note: 60,
                detach: true,
                reset: false,
            },
        );
        let bend = ChannelVoice2::PerNotePitchBend {
            channel: 0,
            note: 60,
            value: 0,
        };
        assert!(run(&mut mpe, bend).is_empty());
        // A new note on the number takes another channel and the bend;
        // the first note-off ends the detached, older note.
        let third = run(&mut mpe, on(60, 0x8000));
        assert_eq!(
            third[0],
            MidiMessage::PitchBend {
                channel: 2,
                value: -8192,
            }
        );
        assert_eq!(
            run(&mut mpe, off(60)),
            [MidiMessage::NoteOff {
                channel: 1,
                note: 60,
                velocity: 64,
            }]
        );
        // A reset puts the number back to rest, on the note's channel too.
        assert_eq!(
            run(
                &mut mpe,
                ChannelVoice2::PerNoteManagement {
                    channel: 0,
                    note: 60,
                    detach: false,
                    reset: true,
                }
            ),
            [
                MidiMessage::PitchBend {
                    channel: 2,
                    value: 0,
                },
                MidiMessage::ChannelPressure {
                    channel: 2,
                    pressure: 0,
                },
                MidiMessage::ControlChange {
                    channel: 2,
                    controller: 74,
                    value: 64,
                },
            ]
        );
        let mut ended = Vec::new();
        mpe.end_all(|message| ended.push(message));
        assert_eq!(
            ended,
            [MidiMessage::NoteOff {
                channel: 2,
                note: 60,
                velocity: 0,
            }]
        );
    }

    #[test]
    fn all_notes_off_forgets_the_notes_and_reset_configures_again() {
        let mut mpe = translator();
        run(&mut mpe, on(60, 0x8000));
        let sent = run(
            &mut mpe,
            ChannelVoice2::ControlChange {
                channel: 0,
                controller: 123,
                value: 0,
            },
        );
        assert_eq!(
            sent,
            [MidiMessage::ControlChange {
                channel: 0,
                controller: 123,
                value: 0,
            }]
        );
        assert!(run(&mut mpe, off(60)).is_empty());
        mpe.reset();
        assert_eq!(run(&mut mpe, off(60)).len(), 3);
    }

    #[test]
    fn a_receiver_reads_the_notes_back_with_their_dimensions() {
        // Through `NoteTracker`, the receiving side of this crate.
        let mut mpe = Midi2ToMpe::new(Zone::lower(15).unwrap());
        let mut tracker = NoteTracker::<16>::new();
        let mut started = Vec::new();
        let mut feed = |mpe: &mut Midi2ToMpe, message| {
            mpe.translate(message, |midi| {
                tracker.feed(midi, |event| {
                    if let NoteEvent::Started(note) = event {
                        started.push(note);
                    }
                });
            });
        };
        for (note, semitones) in [(60u8, 12.0f64), (64, -6.0)] {
            // 12 semitones of 48 is a quarter of the way; -6 an eighth down.
            let position = semitones / 48.0;
            let value = if position >= 0.0 {
                PITCH_BEND_CENTRE + (position * f64::from(u32::MAX - PITCH_BEND_CENTRE)) as u32
            } else {
                PITCH_BEND_CENTRE - (-position * f64::from(PITCH_BEND_CENTRE)) as u32
            };
            feed(
                &mut mpe,
                ChannelVoice2::PerNotePitchBend {
                    channel: 0,
                    note,
                    value,
                },
            );
            feed(&mut mpe, on(note, 0xFFFF));
        }
        assert_eq!(started.len(), 2);
        assert_eq!(started[0].zone, Some(ZoneKind::Lower));
        assert!(
            (started[0].pitch() - 72.0).abs() < 0.01,
            "{}",
            started[0].pitch()
        );
        assert!(
            (started[1].pitch() - 58.0).abs() < 0.01,
            "{}",
            started[1].pitch()
        );
        assert_ne!(started[0].channel, started[1].channel);
    }
}
