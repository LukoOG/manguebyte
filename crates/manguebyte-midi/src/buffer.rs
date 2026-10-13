use crate::MidiMessage;

/// A MIDI message placed at a frame offset inside an audio block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MidiEvent {
    /// Frame, relative to the start of the block, at which the message applies.
    pub offset: u32,
    /// The message.
    pub message: MidiMessage,
}

/// The MIDI messages belonging to one audio block, ordered by frame offset.
///
/// Messages that share an offset keep the order they were pushed in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MidiBuffer {
    events: Vec<MidiEvent>,
}

impl MidiBuffer {
    /// Creates an empty buffer. Does not allocate.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an empty buffer with room for `capacity` events.
    ///
    /// [`push`](Self::push) does not allocate until that room runs out, so
    /// size it generously if you fill it on the audio thread.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            events: Vec::with_capacity(capacity),
        }
    }

    /// Inserts a message at `offset`, keeping the buffer ordered.
    pub fn push(&mut self, offset: u32, message: MidiMessage) {
        let event = MidiEvent { offset, message };
        match self.events.last() {
            Some(last) if last.offset > offset => {
                let index = self.events.partition_point(|e| e.offset <= offset);
                self.events.insert(index, event);
            }
            _ => self.events.push(event),
        }
    }

    /// Removes every event, keeping the allocation.
    pub fn clear(&mut self) {
        self.events.clear();
    }

    /// Number of events.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether the buffer holds no events.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// The events, ordered by offset.
    pub fn as_slice(&self) -> &[MidiEvent] {
        &self.events
    }

    /// Iterates over the events in order.
    pub fn iter(&self) -> core::slice::Iter<'_, MidiEvent> {
        self.events.iter()
    }
}

impl<'a> IntoIterator for &'a MidiBuffer {
    type Item = &'a MidiEvent;
    type IntoIter = core::slice::Iter<'a, MidiEvent>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note_on(note: u8) -> MidiMessage {
        MidiMessage::NoteOn {
            channel: 0,
            note,
            velocity: 100,
        }
    }

    #[test]
    fn keeps_events_ordered_and_stable() {
        let mut buffer = MidiBuffer::new();
        buffer.push(10, note_on(1));
        buffer.push(0, note_on(2));
        buffer.push(10, note_on(3));
        buffer.push(5, note_on(4));
        buffer.push(0, note_on(5));

        let order: Vec<(u32, MidiMessage)> = buffer.iter().map(|e| (e.offset, e.message)).collect();
        assert_eq!(
            order,
            vec![
                (0, note_on(2)),
                (0, note_on(5)),
                (5, note_on(4)),
                (10, note_on(1)),
                (10, note_on(3)),
            ]
        );
    }

    #[test]
    fn clear_empties_the_buffer() {
        let mut buffer = MidiBuffer::with_capacity(4);
        buffer.push(0, note_on(60));
        assert_eq!(buffer.len(), 1);
        buffer.clear();
        assert!(buffer.is_empty());
    }
}
