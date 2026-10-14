use crate::MidiMessage;

/// Up to four channel voice messages, held without allocating.
///
/// One MIDI 2.0 message or one parameter change can stand for several
/// MIDI 1.0 messages that must be sent in order: a registered parameter is
/// four controller messages, a program change with its bank is three.
/// Functions that produce such a group return it as a `MessageList`.
///
/// ```
/// use manguebyte_midi::rpn::{self, Parameter};
/// use manguebyte_midi::MidiMessage;
///
/// let list = rpn::sequence(0, Parameter::PITCH_BEND_SENSITIVITY, 12 << 7);
/// assert_eq!(list.len(), 4);
/// assert_eq!(
///     list.as_slice()[2],
///     MidiMessage::ControlChange { channel: 0, controller: 6, value: 12 }
/// );
/// for message in &list {
///     let _bytes = message.to_bytes();
/// }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MessageList {
    /// Entries past `len` are always [`FILLER`], so derived equality is
    /// exact.
    messages: [MidiMessage; 4],
    len: u8,
}

/// What unused entries hold. Never visible from outside.
const FILLER: MidiMessage = MidiMessage::NoteOff {
    channel: 0,
    note: 0,
    velocity: 0,
};

impl MessageList {
    /// The most messages a list holds.
    pub const CAPACITY: usize = 4;

    /// An empty list.
    pub const fn new() -> Self {
        Self {
            messages: [FILLER; 4],
            len: 0,
        }
    }

    /// The messages, in the order they must be sent.
    #[inline]
    pub fn as_slice(&self) -> &[MidiMessage] {
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
    pub fn iter(&self) -> core::slice::Iter<'_, MidiMessage> {
        self.as_slice().iter()
    }

    /// Adds a message at the end. A fifth message is dropped; nothing in
    /// this crate builds a list that long.
    pub(crate) fn push(&mut self, message: MidiMessage) {
        if let Some(slot) = self.messages.get_mut(usize::from(self.len)) {
            *slot = message;
            self.len += 1;
        }
    }

    pub(crate) fn from_slice(messages: &[MidiMessage]) -> Self {
        let mut list = Self::new();
        for message in messages {
            list.push(*message);
        }
        list
    }
}

impl Default for MessageList {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> IntoIterator for &'a MessageList {
    type Item = &'a MidiMessage;
    type IntoIter = core::slice::Iter<'a, MidiMessage>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(note: u8) -> MidiMessage {
        MidiMessage::NoteOn {
            channel: 0,
            note,
            velocity: 100,
        }
    }

    #[test]
    fn holds_up_to_four_messages_in_order() {
        let mut list = MessageList::new();
        assert!(list.is_empty());
        assert_eq!(list, MessageList::default());
        for key in 0..6 {
            list.push(note(key));
        }
        assert_eq!(list.len(), MessageList::CAPACITY);
        assert_eq!(list.as_slice(), [note(0), note(1), note(2), note(3)]);
        let collected: Vec<MidiMessage> = list.iter().copied().collect();
        assert_eq!(collected, list.as_slice());
        assert_eq!((&list).into_iter().count(), 4);
        assert_eq!(MessageList::from_slice(&[note(0), note(1)]).len(), 2);
    }
}
