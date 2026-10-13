/// A MIDI channel voice message.
///
/// Channels are zero-based (`0..=15`) and data values are 7-bit (`0..=127`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MidiMessage {
    /// A key was released.
    NoteOff {
        /// Channel, `0..=15`.
        channel: u8,
        /// Note number.
        note: u8,
        /// Release velocity.
        velocity: u8,
    },
    /// A key was pressed.
    NoteOn {
        /// Channel, `0..=15`.
        channel: u8,
        /// Note number.
        note: u8,
        /// Attack velocity, `1..=127`.
        velocity: u8,
    },
    /// Per-note aftertouch.
    PolyPressure {
        /// Channel, `0..=15`.
        channel: u8,
        /// Note number.
        note: u8,
        /// Pressure amount.
        pressure: u8,
    },
    /// A controller changed.
    ControlChange {
        /// Channel, `0..=15`.
        channel: u8,
        /// Controller number.
        controller: u8,
        /// Controller value.
        value: u8,
    },
    /// The program (patch) changed.
    ProgramChange {
        /// Channel, `0..=15`.
        channel: u8,
        /// Program number.
        program: u8,
    },
    /// Channel-wide aftertouch.
    ChannelPressure {
        /// Channel, `0..=15`.
        channel: u8,
        /// Pressure amount.
        pressure: u8,
    },
    /// The pitch wheel moved.
    PitchBend {
        /// Channel, `0..=15`.
        channel: u8,
        /// Wheel position, `-8192..=8191`, with `0` at the centre.
        value: i16,
    },
}

/// The wire encoding of a [`MidiMessage`]: two or three bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawMidi {
    bytes: [u8; 3],
    len: u8,
}

impl RawMidi {
    /// The encoded bytes.
    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

impl MidiMessage {
    /// Parses a channel voice message from the start of `bytes`.
    ///
    /// Returns `None` for anything else: system messages, a missing status
    /// byte (running status is not resolved here), truncated input or data
    /// bytes with the high bit set.
    ///
    /// A note-on with velocity zero is returned as a [`NoteOff`], which is
    /// what the MIDI specification says it means.
    ///
    /// [`NoteOff`]: MidiMessage::NoteOff
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let status = *bytes.first()?;
        if !(0x80..0xF0).contains(&status) {
            return None;
        }
        let channel = status & 0x0F;
        let data = |index: usize| bytes.get(index).copied().filter(|byte| *byte < 0x80);

        Some(match status & 0xF0 {
            0x80 => Self::NoteOff {
                channel,
                note: data(1)?,
                velocity: data(2)?,
            },
            0x90 => {
                let (note, velocity) = (data(1)?, data(2)?);
                if velocity == 0 {
                    Self::NoteOff {
                        channel,
                        note,
                        velocity: 0,
                    }
                } else {
                    Self::NoteOn {
                        channel,
                        note,
                        velocity,
                    }
                }
            }
            0xA0 => Self::PolyPressure {
                channel,
                note: data(1)?,
                pressure: data(2)?,
            },
            0xB0 => Self::ControlChange {
                channel,
                controller: data(1)?,
                value: data(2)?,
            },
            0xC0 => Self::ProgramChange {
                channel,
                program: data(1)?,
            },
            0xD0 => Self::ChannelPressure {
                channel,
                pressure: data(1)?,
            },
            0xE0 => {
                let (lsb, msb) = (data(1)?, data(2)?);
                Self::PitchBend {
                    channel,
                    value: ((i16::from(msb) << 7) | i16::from(lsb)) - 8192,
                }
            }
            _ => unreachable!("status was checked to be a channel voice message"),
        })
    }

    /// Encodes the message. Out-of-range fields are masked to fit.
    pub fn to_bytes(&self) -> RawMidi {
        let three = |status: u8, channel: u8, a: u8, b: u8| RawMidi {
            bytes: [status | (channel & 0x0F), a & 0x7F, b & 0x7F],
            len: 3,
        };
        let two = |status: u8, channel: u8, a: u8| RawMidi {
            bytes: [status | (channel & 0x0F), a & 0x7F, 0],
            len: 2,
        };

        match *self {
            Self::NoteOff {
                channel,
                note,
                velocity,
            } => three(0x80, channel, note, velocity),
            Self::NoteOn {
                channel,
                note,
                velocity,
            } => three(0x90, channel, note, velocity),
            Self::PolyPressure {
                channel,
                note,
                pressure,
            } => three(0xA0, channel, note, pressure),
            Self::ControlChange {
                channel,
                controller,
                value,
            } => three(0xB0, channel, controller, value),
            Self::ProgramChange { channel, program } => two(0xC0, channel, program),
            Self::ChannelPressure { channel, pressure } => two(0xD0, channel, pressure),
            Self::PitchBend { channel, value } => {
                let unsigned = (i32::from(value) + 8192).clamp(0, 16383) as u16;
                three(
                    0xE0,
                    channel,
                    (unsigned & 0x7F) as u8,
                    (unsigned >> 7) as u8,
                )
            }
        }
    }

    /// The zero-based channel the message is addressed to.
    pub fn channel(&self) -> u8 {
        match *self {
            Self::NoteOff { channel, .. }
            | Self::NoteOn { channel, .. }
            | Self::PolyPressure { channel, .. }
            | Self::ControlChange { channel, .. }
            | Self::ProgramChange { channel, .. }
            | Self::ChannelPressure { channel, .. }
            | Self::PitchBend { channel, .. } => channel,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_kind() {
        let messages = [
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
            MidiMessage::PitchBend {
                channel: 2,
                value: 0,
            },
        ];
        for message in messages {
            let raw = message.to_bytes();
            assert_eq!(MidiMessage::from_bytes(raw.as_slice()), Some(message));
            assert_eq!(message.channel(), raw.as_slice()[0] & 0x0F);
        }
    }

    #[test]
    fn encodes_expected_bytes() {
        let note_on = MidiMessage::NoteOn {
            channel: 2,
            note: 60,
            velocity: 100,
        };
        assert_eq!(note_on.to_bytes().as_slice(), &[0x92, 60, 100]);

        let program = MidiMessage::ProgramChange {
            channel: 0,
            program: 5,
        };
        assert_eq!(program.to_bytes().as_slice(), &[0xC0, 5]);

        let centred = MidiMessage::PitchBend {
            channel: 0,
            value: 0,
        };
        assert_eq!(centred.to_bytes().as_slice(), &[0xE0, 0x00, 0x40]);
    }

    #[test]
    fn note_on_with_zero_velocity_is_note_off() {
        assert_eq!(
            MidiMessage::from_bytes(&[0x90, 60, 0]),
            Some(MidiMessage::NoteOff {
                channel: 0,
                note: 60,
                velocity: 0
            })
        );
    }

    #[test]
    fn rejects_malformed_input() {
        assert_eq!(MidiMessage::from_bytes(&[]), None);
        assert_eq!(MidiMessage::from_bytes(&[60, 100]), None);
        assert_eq!(MidiMessage::from_bytes(&[0x90, 60]), None);
        assert_eq!(MidiMessage::from_bytes(&[0x90, 0x80, 100]), None);
        assert_eq!(MidiMessage::from_bytes(&[0xF8]), None);
    }
}
