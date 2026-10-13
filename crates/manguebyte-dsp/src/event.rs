//! Things that happen at an exact moment inside a block.
//!
//! Audio is processed in blocks, but music does not line up with them: a note
//! may start on frame 137 of a 512-frame block, and a parameter may be
//! automated to a new value on frame 300. An [`Event`] says what happens and
//! on which frame, and an [`EventBuffer`] carries the events of one block in
//! order.
//!
//! One event type covers everything that reaches a processor in time: notes,
//! per-note expression, parameter changes, modulation and raw MIDI. That
//! keeps them in a single ordered stream, so a note and the parameter change
//! that belongs with it cannot arrive in the wrong order.

use manguebyte_core::ParamId;
use manguebyte_midi::MidiMessage;

/// Which note an event is about.
///
/// A key on a channel is enough for a keyboard. When the same key can sound
/// more than once at a time (two overlapping notes on one string, a host's
/// piano roll with stacked notes), `id` tells them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NoteRef {
    /// Channel, `0..=15`.
    pub channel: u8,
    /// Key number, `0..=127`, with 60 as middle C.
    pub key: u8,
    /// An identifier unique among the notes sounding at the same time, when
    /// the source provides one.
    pub id: Option<u32>,
}

impl NoteRef {
    /// A note identified by channel and key alone.
    pub const fn new(channel: u8, key: u8) -> Self {
        Self {
            channel,
            key,
            id: None,
        }
    }

    /// A note with its own identifier.
    pub const fn with_id(channel: u8, key: u8, id: u32) -> Self {
        Self {
            channel,
            key,
            id: Some(id),
        }
    }

    /// Whether an event addressed to `self` applies to the sounding note
    /// `other`.
    ///
    /// If both carry an identifier, the identifiers decide. Otherwise channel
    /// and key do.
    pub fn matches(&self, other: &NoteRef) -> bool {
        match (self.id, other.id) {
            (Some(a), Some(b)) => a == b,
            _ => self.channel == other.channel && self.key == other.key,
        }
    }
}

/// A dimension in which one note can be shaped while it sounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Expression {
    /// Loudness, as a gain factor where 1 leaves the note unchanged.
    Volume,
    /// Stereo position, from -1 (left) to 1 (right).
    Pan,
    /// Pitch offset in semitones.
    Tuning,
    /// Depth of vibrato, from 0 to 1.
    Vibrato,
    /// Timbre, from 0 (dark) to 1 (bright).
    Brightness,
    /// Pressure on the key after it was struck, from 0 to 1.
    Pressure,
}

/// What happens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EventKind {
    /// A note starts.
    NoteOn {
        /// The note.
        note: NoteRef,
        /// How hard it was struck, from 0 to 1.
        velocity: f32,
    },
    /// A note is released and begins to fade.
    NoteOff {
        /// The note.
        note: NoteRef,
        /// How fast it was released, from 0 to 1.
        velocity: f32,
    },
    /// A note stops at once, with no release.
    NoteChoke {
        /// The note.
        note: NoteRef,
    },
    /// One sounding note is shaped without affecting the others.
    NoteExpression {
        /// The note.
        note: NoteRef,
        /// What is being shaped.
        expression: Expression,
        /// The new value, in the unit documented on [`Expression`].
        value: f64,
    },
    /// A parameter takes a new value, as from automation or a control.
    ParamValue {
        /// The parameter.
        param: ParamId,
        /// The new value, in the parameter's own unit.
        value: f64,
    },
    /// An offset is added on top of a parameter's value without changing the
    /// value itself, as from an LFO or an envelope in the host.
    ///
    /// Because the value is untouched, modulation never fights automation
    /// and never ends up saved in a preset.
    ParamModulation {
        /// The parameter.
        param: ParamId,
        /// The offset, in the parameter's own unit. Zero ends the modulation.
        amount: f64,
        /// The single note the offset applies to, or `None` for all of them.
        note: Option<NoteRef>,
    },
    /// A MIDI message with no richer equivalent above: controllers, pitch
    /// bend, program changes, pressure.
    Midi(MidiMessage),
}

/// Something that happens on one frame of a block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Event {
    /// Frame within the block, counted from its start.
    pub offset: u32,
    /// What happens.
    pub kind: EventKind,
}

impl Event {
    /// Converts a MIDI message into the richest event that represents it.
    ///
    /// Note-ons and note-offs become note events with velocity scaled to
    /// `0..=1`, and per-note pressure becomes a note expression. Everything
    /// else is carried as [`EventKind::Midi`].
    pub fn from_midi(offset: u32, message: MidiMessage) -> Self {
        let kind = match message {
            MidiMessage::NoteOn {
                channel,
                note,
                velocity,
            } => EventKind::NoteOn {
                note: NoteRef::new(channel, note),
                velocity: f32::from(velocity) / 127.0,
            },
            MidiMessage::NoteOff {
                channel,
                note,
                velocity,
            } => EventKind::NoteOff {
                note: NoteRef::new(channel, note),
                velocity: f32::from(velocity) / 127.0,
            },
            MidiMessage::PolyPressure {
                channel,
                note,
                pressure,
            } => EventKind::NoteExpression {
                note: NoteRef::new(channel, note),
                expression: Expression::Pressure,
                value: f64::from(pressure) / 127.0,
            },
            other => EventKind::Midi(other),
        };
        Self { offset, kind }
    }
}

/// The events of one block, in the order they happen.
///
/// The buffer has a fixed capacity, decided when it is created, and never
/// allocates afterwards. Events are kept sorted by frame; events on the same
/// frame stay in the order they were added.
///
/// ```
/// use manguebyte_dsp::event::{Event, EventBuffer, EventKind, NoteRef};
///
/// let mut events = EventBuffer::with_capacity(16);
/// let note = NoteRef::new(0, 60);
/// events.push(Event { offset: 200, kind: EventKind::NoteOff { note, velocity: 0.0 } }).unwrap();
/// events.push(Event { offset: 10, kind: EventKind::NoteOn { note, velocity: 0.8 } }).unwrap();
///
/// let offsets: Vec<u32> = events.iter().map(|event| event.offset).collect();
/// assert_eq!(offsets, [10, 200]);
/// ```
#[derive(Debug, Default, PartialEq)]
pub struct EventBuffer {
    events: Vec<Event>,
    capacity: usize,
}

impl EventBuffer {
    /// A buffer that holds nothing and never will. Useful as a placeholder
    /// for a block with no events.
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

    /// Adds an event, or hands it back if the buffer is full. Real-time safe.
    pub fn push(&mut self, event: Event) -> Result<(), Event> {
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

    /// Adds a MIDI message, converted by [`Event::from_midi`]. Real-time safe.
    pub fn push_midi(&mut self, offset: u32, message: MidiMessage) -> Result<(), Event> {
        self.push(Event::from_midi(offset, message))
    }

    /// Removes every event. Real-time safe.
    pub fn clear(&mut self) {
        self.events.clear();
    }

    /// How many events the buffer holds.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether the buffer holds no events.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// How many events the buffer can hold.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// The events, in order.
    pub fn as_slice(&self) -> &[Event] {
        &self.events
    }

    /// Iterates over the events, in order.
    pub fn iter(&self) -> core::slice::Iter<'_, Event> {
        self.events.iter()
    }
}

impl<'a> IntoIterator for &'a EventBuffer {
    type Item = &'a Event;
    type IntoIter = core::slice::Iter<'a, Event>;

    fn into_iter(self) -> Self::IntoIter {
        self.events.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(offset: u32, key: u8) -> Event {
        Event {
            offset,
            kind: EventKind::NoteOn {
                note: NoteRef::new(0, key),
                velocity: 1.0,
            },
        }
    }

    fn keys(events: &EventBuffer) -> Vec<u8> {
        events
            .iter()
            .map(|event| match event.kind {
                EventKind::NoteOn { note, .. } => note.key,
                _ => unreachable!(),
            })
            .collect()
    }

    #[test]
    fn keeps_time_order_and_insertion_order_within_a_frame() {
        let mut events = EventBuffer::with_capacity(8);
        for (offset, key) in [(50, 1), (10, 2), (50, 3), (10, 4), (0, 5), (99, 6)] {
            events.push(on(offset, key)).unwrap();
        }
        assert_eq!(keys(&events), [5, 2, 4, 1, 3, 6]);
        assert_eq!(events.len(), 6);
        events.clear();
        assert!(events.is_empty());
        assert_eq!(events.capacity(), 8);
    }

    #[test]
    fn a_full_buffer_hands_the_event_back() {
        let mut events = EventBuffer::with_capacity(2);
        events.push(on(0, 1)).unwrap();
        events.push(on(0, 2)).unwrap();
        assert_eq!(events.push(on(0, 3)), Err(on(0, 3)));
        assert_eq!(EventBuffer::new().push(on(0, 1)), Err(on(0, 1)));
    }

    #[test]
    fn midi_becomes_the_richest_matching_event() {
        let note = NoteRef::new(3, 64);
        let converted = |message| Event::from_midi(7, message).kind;
        assert_eq!(
            converted(MidiMessage::NoteOn {
                channel: 3,
                note: 64,
                velocity: 127
            }),
            EventKind::NoteOn {
                note,
                velocity: 1.0
            }
        );
        assert_eq!(
            converted(MidiMessage::NoteOff {
                channel: 3,
                note: 64,
                velocity: 0
            }),
            EventKind::NoteOff {
                note,
                velocity: 0.0
            }
        );
        assert_eq!(
            converted(MidiMessage::PolyPressure {
                channel: 3,
                note: 64,
                pressure: 127
            }),
            EventKind::NoteExpression {
                note,
                expression: Expression::Pressure,
                value: 1.0
            }
        );
        let bend = MidiMessage::PitchBend {
            channel: 3,
            value: 100,
        };
        assert_eq!(converted(bend), EventKind::Midi(bend));
    }

    #[test]
    fn note_references_match_by_id_when_both_have_one() {
        let plain = NoteRef::new(0, 60);
        let first = NoteRef::with_id(0, 60, 1);
        let second = NoteRef::with_id(0, 60, 2);
        assert!(plain.matches(&first) && first.matches(&plain));
        assert!(first.matches(&first));
        assert!(!first.matches(&second));
        assert!(!plain.matches(&NoteRef::new(0, 61)));
        assert!(!plain.matches(&NoteRef::new(1, 60)));
        // An id is decisive even if the key was reported differently.
        assert!(first.matches(&NoteRef::with_id(5, 72, 1)));
    }
}
