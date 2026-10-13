use crate::MidiMessage;

/// A MIDI system message: one that is not addressed to a channel.
///
/// System exclusive messages have no fixed size and are delivered separately,
/// as [`StreamEvent::SysEx`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemMessage {
    /// One eighth of a MIDI time code position.
    TimeCodeQuarterFrame {
        /// Which piece of the time code this is, `0..=7`.
        piece: u8,
        /// The four bits of that piece.
        value: u8,
    },
    /// Where the song is, in sixteenth notes from the start, `0..=16383`.
    SongPosition(u16),
    /// Which song or sequence to play.
    SongSelect(u8),
    /// Asks analogue synthesisers to tune their oscillators.
    TuneRequest,
    /// Sent 24 times per quarter note while a sequence plays.
    TimingClock,
    /// Start playing from the beginning.
    Start,
    /// Carry on playing from where it stopped.
    Continue,
    /// Stop playing.
    Stop,
    /// Sent repeatedly to show the connection is alive.
    ActiveSensing,
    /// Return to the power-on state.
    Reset,
}

impl SystemMessage {
    /// Whether this is a real-time message, which may appear anywhere in a
    /// stream, even between the bytes of another message.
    pub fn is_real_time(&self) -> bool {
        matches!(
            self,
            Self::TimingClock
                | Self::Start
                | Self::Continue
                | Self::Stop
                | Self::ActiveSensing
                | Self::Reset
        )
    }

    /// Encodes the message into `out` and returns how many bytes were
    /// written: one, two or three.
    pub fn write(&self, out: &mut [u8; 3]) -> usize {
        match *self {
            Self::TimeCodeQuarterFrame { piece, value } => {
                *out = [0xF1, ((piece & 0x07) << 4) | (value & 0x0F), 0];
                2
            }
            Self::SongPosition(position) => {
                *out = [
                    0xF2,
                    (position & 0x7F) as u8,
                    ((position >> 7) & 0x7F) as u8,
                ];
                3
            }
            Self::SongSelect(song) => {
                *out = [0xF3, song & 0x7F, 0];
                2
            }
            Self::TuneRequest => single(out, 0xF6),
            Self::TimingClock => single(out, 0xF8),
            Self::Start => single(out, 0xFA),
            Self::Continue => single(out, 0xFB),
            Self::Stop => single(out, 0xFC),
            Self::ActiveSensing => single(out, 0xFE),
            Self::Reset => single(out, 0xFF),
        }
    }

    fn real_time(status: u8) -> Option<Self> {
        Some(match status {
            0xF8 => Self::TimingClock,
            0xFA => Self::Start,
            0xFB => Self::Continue,
            0xFC => Self::Stop,
            0xFE => Self::ActiveSensing,
            0xFF => Self::Reset,
            _ => return None,
        })
    }
}

fn single(out: &mut [u8; 3], status: u8) -> usize {
    *out = [status, 0, 0];
    1
}

/// One complete item decoded from a MIDI byte stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamEvent<'a> {
    /// A channel voice message.
    Channel(MidiMessage),
    /// A system common or real-time message.
    System(SystemMessage),
    /// The payload of a system exclusive message, without the `0xF0` that
    /// starts it or the `0xF7` that ends it.
    SysEx(&'a [u8]),
}

/// Decodes a stream of MIDI bytes, as it arrives from a cable or a driver.
///
/// A raw stream is harder to read than a list of messages: a status byte may
/// be omitted when it repeats ("running status"), real-time messages may cut
/// into the middle of another message, and system exclusive data can be any
/// length. This parser handles all three.
///
/// Feeding bytes is real-time safe. The buffer for system exclusive data is
/// allocated when the parser is created; a message that does not fit is
/// dropped whole rather than delivered cut short.
///
/// ```
/// use manguebyte_midi::{MidiMessage, StreamEvent, StreamParser};
///
/// let mut parser = StreamParser::new(256);
/// let mut notes = Vec::new();
///
/// // Two note-ons, the second relying on running status, with a timing
/// // clock byte (0xF8) arriving in the middle of the first.
/// for byte in [0x90, 60, 0xF8, 100, 64, 90] {
///     parser.feed(byte, |event| {
///         if let StreamEvent::Channel(message) = event {
///             notes.push(message);
///         }
///     });
/// }
/// assert_eq!(notes, [
///     MidiMessage::NoteOn { channel: 0, note: 60, velocity: 100 },
///     MidiMessage::NoteOn { channel: 0, note: 64, velocity: 90 },
/// ]);
/// ```
// Not `Clone`: a cloned `Vec` keeps only as much capacity as it has items,
// so a copy would allocate the first time system exclusive data arrived.
#[derive(Debug)]
pub struct StreamParser {
    /// Status the next data bytes belong to, or zero for none.
    status: u8,
    data: [u8; 2],
    data_len: usize,
    sysex: Vec<u8>,
    sysex_capacity: usize,
    in_sysex: bool,
    sysex_overflowed: bool,
}

impl StreamParser {
    /// Creates a parser that can hold system exclusive messages of up to
    /// `sysex_capacity` bytes. Allocates.
    pub fn new(sysex_capacity: usize) -> Self {
        Self {
            status: 0,
            data: [0; 2],
            data_len: 0,
            sysex: Vec::with_capacity(sysex_capacity),
            sysex_capacity,
            in_sysex: false,
            sysex_overflowed: false,
        }
    }

    /// Forgets any partly received message and the running status.
    pub fn reset(&mut self) {
        self.status = 0;
        self.data_len = 0;
        self.sysex.clear();
        self.in_sysex = false;
        self.sysex_overflowed = false;
    }

    /// Takes the next byte of the stream and calls `on_event` if it completes
    /// a message. Real-time safe as long as `on_event` is.
    pub fn feed(&mut self, byte: u8, mut on_event: impl FnMut(StreamEvent<'_>)) {
        // Real-time messages are a single byte and leave everything else
        // exactly as it was, including a message they interrupt.
        if byte >= 0xF8 {
            if let Some(message) = SystemMessage::real_time(byte) {
                on_event(StreamEvent::System(message));
            }
            return;
        }

        if byte >= 0x80 {
            // Any other status byte ends a system exclusive message. Only
            // 0xF7 ends it properly; anything else means it was abandoned.
            if self.in_sysex {
                self.in_sysex = false;
                if byte == 0xF7 && !self.sysex_overflowed {
                    on_event(StreamEvent::SysEx(&self.sysex));
                }
            }
            self.data_len = 0;
            match byte {
                0xF0 => {
                    self.sysex.clear();
                    self.in_sysex = true;
                    self.sysex_overflowed = false;
                    self.status = 0;
                }
                0xF6 => {
                    self.status = 0;
                    on_event(StreamEvent::System(SystemMessage::TuneRequest));
                }
                // System common messages cancel running status. 0xF4 and 0xF5
                // are undefined and 0xF7 was handled above.
                0xF4 | 0xF5 | 0xF7 => self.status = 0,
                _ => self.status = byte,
            }
            return;
        }

        // A data byte.
        if self.in_sysex {
            if self.sysex.len() < self.sysex_capacity {
                self.sysex.push(byte);
            } else {
                self.sysex_overflowed = true;
            }
            return;
        }
        if self.status == 0 {
            // Data with no status to attach it to: noise, or the tail of a
            // message whose start we never saw.
            return;
        }

        self.data[self.data_len] = byte;
        self.data_len += 1;
        if self.data_len < data_bytes(self.status) {
            return;
        }
        self.data_len = 0;

        let [first, second] = self.data;
        if self.status < 0xF0 {
            let bytes = [self.status, first, second];
            if let Some(message) = MidiMessage::from_bytes(&bytes) {
                on_event(StreamEvent::Channel(message));
            }
            // Running status: the next data bytes reuse this status.
        } else {
            let message = match self.status {
                0xF1 => SystemMessage::TimeCodeQuarterFrame {
                    piece: first >> 4,
                    value: first & 0x0F,
                },
                0xF2 => SystemMessage::SongPosition(u16::from(first) | (u16::from(second) << 7)),
                _ => SystemMessage::SongSelect(first),
            };
            self.status = 0;
            on_event(StreamEvent::System(message));
        }
    }

    /// Feeds every byte of `bytes` in order.
    pub fn feed_slice(&mut self, bytes: &[u8], mut on_event: impl FnMut(StreamEvent<'_>)) {
        for &byte in bytes {
            self.feed(byte, &mut on_event);
        }
    }
}

/// How many data bytes follow a status byte.
fn data_bytes(status: u8) -> usize {
    match status {
        0xC0..=0xDF | 0xF1 | 0xF3 => 1,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An owned copy of a stream event, so tests can collect them.
    #[derive(Debug, PartialEq)]
    enum Seen {
        Channel(MidiMessage),
        System(SystemMessage),
        SysEx(Vec<u8>),
    }

    fn parse(bytes: &[u8]) -> Vec<Seen> {
        parse_with(StreamParser::new(64), bytes)
    }

    fn parse_with(mut parser: StreamParser, bytes: &[u8]) -> Vec<Seen> {
        let mut seen = Vec::new();
        parser.feed_slice(bytes, |event| {
            seen.push(match event {
                StreamEvent::Channel(message) => Seen::Channel(message),
                StreamEvent::System(message) => Seen::System(message),
                StreamEvent::SysEx(data) => Seen::SysEx(data.to_vec()),
            });
        });
        seen
    }

    fn note_on(note: u8, velocity: u8) -> Seen {
        Seen::Channel(MidiMessage::NoteOn {
            channel: 2,
            note,
            velocity,
        })
    }

    #[test]
    fn running_status_carries_across_messages_of_both_sizes() {
        assert_eq!(
            parse(&[0x92, 60, 100, 62, 101, 64, 102]),
            [note_on(60, 100), note_on(62, 101), note_on(64, 102)]
        );
        let program = |program| {
            Seen::Channel(MidiMessage::ProgramChange {
                channel: 5,
                program,
            })
        };
        assert_eq!(
            parse(&[0xC5, 1, 2, 3]),
            [program(1), program(2), program(3)]
        );
    }

    #[test]
    fn real_time_bytes_interleave_without_disturbing_anything() {
        assert_eq!(
            parse(&[0x92, 0xF8, 60, 0xFA, 100, 0xFE, 62, 0xF8, 101]),
            [
                Seen::System(SystemMessage::TimingClock),
                Seen::System(SystemMessage::Start),
                note_on(60, 100),
                Seen::System(SystemMessage::ActiveSensing),
                Seen::System(SystemMessage::TimingClock),
                note_on(62, 101),
            ]
        );
    }

    #[test]
    fn system_common_messages_decode_and_cancel_running_status() {
        assert_eq!(
            parse(&[
                0x92, 60, 100, 0xF2, 0x05, 0x02, 62, 101, 0xF3, 9, 0xF1, 0x35, 0xF6
            ]),
            [
                note_on(60, 100),
                Seen::System(SystemMessage::SongPosition(5 + (2 << 7))),
                // 62, 101 has no status any more and is ignored.
                Seen::System(SystemMessage::SongSelect(9)),
                Seen::System(SystemMessage::TimeCodeQuarterFrame { piece: 3, value: 5 }),
                Seen::System(SystemMessage::TuneRequest),
            ]
        );
    }

    #[test]
    fn sysex_is_delivered_whole_even_with_real_time_inside() {
        assert_eq!(
            parse(&[0xF0, 0x7E, 0x7F, 0xF8, 0x06, 0x01, 0xF7, 0x92, 60, 100]),
            [
                Seen::System(SystemMessage::TimingClock),
                Seen::SysEx(vec![0x7E, 0x7F, 0x06, 0x01]),
                note_on(60, 100),
            ]
        );
        assert_eq!(parse(&[0xF0, 0xF7]), [Seen::SysEx(vec![])]);
    }

    #[test]
    fn abandoned_or_oversized_sysex_is_dropped_not_truncated() {
        // Interrupted by a note-on: the sysex is lost, the note is not.
        assert_eq!(parse(&[0xF0, 1, 2, 0x92, 60, 100]), [note_on(60, 100)]);

        let small = StreamParser::new(2);
        assert_eq!(
            parse_with(small, &[0xF0, 1, 2, 3, 0xF7, 0xF0, 4, 5, 0xF7]),
            [Seen::SysEx(vec![4, 5])]
        );
    }

    #[test]
    fn stray_bytes_are_ignored() {
        assert_eq!(parse(&[60, 100, 0xF7, 0xF4, 0xF5, 0xF9, 0xFD, 12]), []);
        // A message cut short by another status byte is dropped.
        assert_eq!(parse(&[0x92, 60, 0x92, 62, 101]), [note_on(62, 101)]);
    }

    #[test]
    fn reset_forgets_running_status() {
        let mut parser = StreamParser::new(8);
        parser.feed_slice(&[0x92, 60], |_| {});
        parser.reset();
        assert_eq!(parse_with(parser, &[100, 62, 101]), []);
    }

    #[test]
    fn system_messages_encode_to_what_the_parser_reads() {
        for message in [
            SystemMessage::TimeCodeQuarterFrame {
                piece: 7,
                value: 15,
            },
            SystemMessage::SongPosition(16_383),
            SystemMessage::SongPosition(0),
            SystemMessage::SongSelect(127),
            SystemMessage::TuneRequest,
            SystemMessage::TimingClock,
            SystemMessage::Start,
            SystemMessage::Continue,
            SystemMessage::Stop,
            SystemMessage::ActiveSensing,
            SystemMessage::Reset,
        ] {
            let mut bytes = [0; 3];
            let len = message.write(&mut bytes);
            assert_eq!(parse(&bytes[..len]), [Seen::System(message)]);
            assert_eq!(message.is_real_time(), bytes[0] >= 0xF8);
        }
    }
}
