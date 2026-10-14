//! Flex data messages: message type `0xD`, section 7.5 of the
//! specification. Tempo, time signature, metronome, key signature, chord
//! names and text.

use super::{Packet, Part, Pieces, UmpError, bytes16, header, nibble, octet, until_nul};

/// What a flex data message applies to.
///
/// Two of the four values of this field are reserved, so a variant may be
/// added and a `match` on this type needs a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FlexAddress {
    /// One channel of the group, `0..=15`.
    Channel(u8),
    /// The whole group.
    Group,
}

/// A flex data message (section 7.5).
///
/// When [`Packet::message`] reports an unknown flex data status, the
/// error's `status` holds the status bank in its high byte and the status
/// in its low byte.
///
/// ```
/// use manguebyte_midi::ump::{FlexAddress, FlexData, FlexMessage, Message};
///
/// let tempo = FlexData {
///     group: 0,
///     address: FlexAddress::Group,
///     message: FlexMessage::tempo_from_bpm(120.0),
/// };
/// let packet = Message::Flex(tempo).to_packet();
/// // 120 beats per minute is 50 000 000 units of ten nanoseconds.
/// assert_eq!(packet.words(), &[0xD010_0000, 50_000_000, 0, 0]);
/// assert_eq!(tempo.message.tempo_bpm(), Some(120.0));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FlexData {
    /// Group, `0..=15`.
    pub group: u8,
    /// What the message applies to. Tempo, time signature and metronome
    /// are meant for the whole group.
    pub address: FlexAddress,
    /// The message.
    pub message: FlexMessage,
}

/// The content of a flex data message.
///
/// Most statuses and all but three status banks are reserved, so more
/// variants may be added and a `match` on this type needs a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FlexMessage {
    /// The tempo (section 7.5.3).
    SetTempo {
        /// Length of a quarter note in units of ten nanoseconds.
        ten_nanoseconds_per_quarter: u32,
    },
    /// The time signature of the bars that follow (section 7.5.4).
    SetTimeSignature {
        /// Beats in a bar. The specification gives the range as 1 to 256
        /// without saying how 256 is written in eight bits; the byte is
        /// passed through as it is.
        numerator: u8,
        /// The note value of a beat as a power of two: 2 means a quarter
        /// note, 3 an eighth. Zero means a denominator that is not a power
        /// of two.
        denominator_power: u8,
        /// Thirty-second notes in 24 MIDI clocks, normally 8.
        thirty_seconds_per_quarter: u8,
    },
    /// How a metronome should click (section 7.5.5).
    SetMetronome(Metronome),
    /// The key signature (section 7.5.7).
    SetKeySignature {
        /// Number of sharps, or of flats when negative, `-7..=7`. `-8`
        /// means unknown or not a standard key signature.
        sharps: i8,
        /// The tonic: 1 is A, 2 is B and so on to 7 for G; 0 is unknown.
        /// Whether it is natural, sharp or flat follows from `sharps`.
        tonic: u8,
    },
    /// The name of the chord being played (section 7.5.8).
    SetChordName(ChordName),
    /// Up to twelve bytes of text (section 7.5.9).
    Text(FlexText),
}

impl FlexMessage {
    /// A tempo given in quarter notes per minute.
    ///
    /// The format holds tempos from about 1.397 beats per minute upwards;
    /// slower ones, and values that are not positive numbers, give the
    /// slowest tempo it can express.
    pub fn tempo_from_bpm(beats_per_minute: f64) -> Self {
        let units = 6.0e9 / beats_per_minute;
        Self::SetTempo {
            ten_nanoseconds_per_quarter: if units >= 1.0 {
                // `as` saturates at the largest value.
                units.round() as u32
            } else if units >= 0.0 {
                1
            } else {
                u32::MAX
            },
        }
    }

    /// The tempo in quarter notes per minute, if this is a tempo message
    /// with a length above zero.
    pub fn tempo_bpm(&self) -> Option<f64> {
        match *self {
            Self::SetTempo {
                ten_nanoseconds_per_quarter: units,
            } if units > 0 => Some(6.0e9 / f64::from(units)),
            _ => None,
        }
    }
}

/// Metronome settings (section 7.5.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Metronome {
    /// MIDI clocks between the main clicks: 24 for a click on every
    /// quarter note.
    pub clocks_per_primary_click: u8,
    /// How the bar is divided for accents. An undivided bar holds its
    /// number of beats in the first part and zero in the others; 5/4 as
    /// three plus two is `[3, 2, 0]`.
    pub bar_accents: [u8; 3],
    /// Extra clicks inside each main click, for two layers of
    /// subdivision. Zero means none.
    pub subdivision_clicks: [u8; 2],
}

/// One change made to a chord: a kind and the degree it applies to
/// (section 7.5.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Alteration {
    /// 0 for none, 1 to add the degree, 2 to take it away, 3 to raise it
    /// and 4 to lower it. Four bits.
    pub kind: u8,
    /// The degree of the chord: 1 for the root, 3 for the third and so on.
    /// Four bits.
    pub degree: u8,
}

impl Alteration {
    /// No alteration.
    pub const NONE: Self = Self { kind: 0, degree: 0 };
    /// Kind: add the degree.
    pub const ADD: u8 = 1;
    /// Kind: take the degree away.
    pub const SUBTRACT: u8 = 2;
    /// Kind: raise the degree, adding it if it is not there.
    pub const RAISE: u8 = 3;
    /// Kind: lower the degree, adding it if it is not there.
    pub const LOWER: u8 = 4;
}

/// Values of [`ChordName::chord_type`] (table 14 of the specification).
pub mod chord_type {
    /// No chord: clears the chord name.
    pub const NONE: u8 = 0x00;
    /// Major triad.
    pub const MAJOR: u8 = 0x01;
    /// Major sixth.
    pub const MAJOR_6TH: u8 = 0x02;
    /// Major seventh.
    pub const MAJOR_7TH: u8 = 0x03;
    /// Major ninth.
    pub const MAJOR_9TH: u8 = 0x04;
    /// Major eleventh.
    pub const MAJOR_11TH: u8 = 0x05;
    /// Major thirteenth.
    pub const MAJOR_13TH: u8 = 0x06;
    /// Minor triad.
    pub const MINOR: u8 = 0x07;
    /// Minor sixth.
    pub const MINOR_6TH: u8 = 0x08;
    /// Minor seventh.
    pub const MINOR_7TH: u8 = 0x09;
    /// Minor ninth.
    pub const MINOR_9TH: u8 = 0x0A;
    /// Minor eleventh.
    pub const MINOR_11TH: u8 = 0x0B;
    /// Minor thirteenth.
    pub const MINOR_13TH: u8 = 0x0C;
    /// Dominant seventh.
    pub const DOMINANT: u8 = 0x0D;
    /// Dominant ninth.
    pub const DOMINANT_9TH: u8 = 0x0E;
    /// Dominant eleventh.
    pub const DOMINANT_11TH: u8 = 0x0F;
    /// Dominant thirteenth.
    pub const DOMINANT_13TH: u8 = 0x10;
    /// Augmented triad.
    pub const AUGMENTED: u8 = 0x11;
    /// Augmented seventh.
    pub const AUGMENTED_7TH: u8 = 0x12;
    /// Diminished triad.
    pub const DIMINISHED: u8 = 0x13;
    /// Diminished seventh.
    pub const DIMINISHED_7TH: u8 = 0x14;
    /// Half diminished: diminished triad with a minor seventh.
    pub const HALF_DIMINISHED: u8 = 0x15;
    /// Minor triad with a major seventh.
    pub const MAJOR_MINOR: u8 = 0x16;
    /// Pedal: root and octave.
    pub const PEDAL: u8 = 0x17;
    /// Power chord: root and fifth.
    pub const POWER: u8 = 0x18;
    /// Suspended second.
    pub const SUSPENDED_2ND: u8 = 0x19;
    /// Suspended fourth.
    pub const SUSPENDED_4TH: u8 = 0x1A;
    /// Seventh with a suspended fourth.
    pub const SEVEN_SUSPENDED_4TH: u8 = 0x1B;
}

/// The name of a chord, with an optional bass note and bass chord
/// (section 7.5.8).
///
/// Note letters are numbers: 1 is A, 2 is B and so on to 7 for G.
/// Accidentals are counted: 1 is a sharp, 2 a double sharp, -1 a flat and
/// -2 a double flat.
///
/// ```
/// use manguebyte_midi::ump::{chord_type, ChordName, FlexAddress, FlexData, FlexMessage, Message};
///
/// // B flat minor, the first example of figure 76.
/// let chord = ChordName::new(2, -1, chord_type::MINOR);
/// let packet = Message::Flex(FlexData {
///     group: 0,
///     address: FlexAddress::Group,
///     message: FlexMessage::SetChordName(chord),
/// })
/// .to_packet();
/// assert_eq!(packet.words(), &[0xD010_0006, 0xF207_0000, 0, 0x8000_0000]);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChordName {
    /// Sharps (positive) or flats (negative) on the tonic, `-2..=2`.
    pub tonic_accidentals: i8,
    /// The tonic's letter, `1..=7`, or 0 for unknown or no chord.
    pub tonic: u8,
    /// The kind of chord: one of the [`chord_type`] values.
    pub chord_type: u8,
    /// Up to four changes to the chord. Unused entries are
    /// [`Alteration::NONE`].
    pub alterations: [Alteration; 4],
    /// Sharps or flats on the bass note, `-2..=2`, or
    /// [`BASS_IS_TONIC`](Self::BASS_IS_TONIC) with `bass` at 0 when the
    /// bass note is the tonic.
    pub bass_accidentals: i8,
    /// The bass note's letter, `1..=7`, or 0 when it is the tonic.
    pub bass: u8,
    /// The kind of chord built on the bass note, or
    /// [`chord_type::NONE`].
    pub bass_chord_type: u8,
    /// Up to two changes to the bass chord.
    pub bass_alterations: [Alteration; 2],
}

impl ChordName {
    /// The value of [`bass_accidentals`](Self::bass_accidentals) that says
    /// the bass note is the chord's tonic.
    pub const BASS_IS_TONIC: i8 = -8;

    /// A chord on `tonic` with no alterations, sounding over its own
    /// tonic.
    pub const fn new(tonic: u8, tonic_accidentals: i8, chord_type: u8) -> Self {
        Self {
            tonic_accidentals,
            tonic,
            chord_type,
            alterations: [Alteration::NONE; 4],
            bass_accidentals: Self::BASS_IS_TONIC,
            bass: 0,
            bass_chord_type: chord_type::NONE,
            bass_alterations: [Alteration::NONE; 2],
        }
    }
}

/// Which text a [`FlexText`] carries (table 16 of the specification).
///
/// The table has room for more kinds, so variants may be added and a
/// `match` on this type needs a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TextKind {
    /// Metadata of no particular kind.
    UnknownMetadata,
    /// The name of the project.
    ProjectName,
    /// The name of the composition or song.
    CompositionName,
    /// The name of the MIDI clip.
    ClipName,
    /// A copyright notice.
    Copyright,
    /// Who composed the music.
    Composer,
    /// Who wrote the lyrics.
    Lyricist,
    /// Who arranged the music.
    Arranger,
    /// Who published it.
    Publisher,
    /// The main performer.
    PrimaryPerformer,
    /// An accompanying performer.
    AccompanyingPerformer,
    /// The date of the recording or concert, as ISO 8601.
    RecordingDate,
    /// Where the recording or concert took place.
    RecordingLocation,
    /// Performance text of no particular kind.
    UnknownPerformanceText,
    /// A syllable of lyrics, sent when its note begins.
    Lyrics,
    /// The language of the lyrics that follow, as a BCP 47 tag.
    LyricsLanguage,
    /// A syllable of ruby text: small annotations beside the lyrics.
    Ruby,
    /// The language of the ruby text that follows, as a BCP 47 tag.
    RubyLanguage,
}

impl TextKind {
    const ALL: [(Self, u8, u8); 18] = [
        (Self::UnknownMetadata, 0x01, 0x00),
        (Self::ProjectName, 0x01, 0x01),
        (Self::CompositionName, 0x01, 0x02),
        (Self::ClipName, 0x01, 0x03),
        (Self::Copyright, 0x01, 0x04),
        (Self::Composer, 0x01, 0x05),
        (Self::Lyricist, 0x01, 0x06),
        (Self::Arranger, 0x01, 0x07),
        (Self::Publisher, 0x01, 0x08),
        (Self::PrimaryPerformer, 0x01, 0x09),
        (Self::AccompanyingPerformer, 0x01, 0x0A),
        (Self::RecordingDate, 0x01, 0x0B),
        (Self::RecordingLocation, 0x01, 0x0C),
        (Self::UnknownPerformanceText, 0x02, 0x00),
        (Self::Lyrics, 0x02, 0x01),
        (Self::LyricsLanguage, 0x02, 0x02),
        (Self::Ruby, 0x02, 0x03),
        (Self::RubyLanguage, 0x02, 0x04),
    ];

    /// The status bank and status that identify this kind of text in a
    /// packet.
    pub fn bank_and_status(self) -> (u8, u8) {
        Self::ALL
            .iter()
            .find(|(kind, _, _)| *kind == self)
            .map_or((0x01, 0x00), |&(_, bank, status)| (bank, status))
    }

    /// The kind of text a status bank and status stand for, if defined.
    pub fn from_bank_and_status(bank: u8, status: u8) -> Option<Self> {
        Self::ALL
            .iter()
            .find(|&&(_, b, s)| (b, s) == (bank, status))
            .map(|&(kind, _, _)| kind)
    }
}

/// One packet of text: up to twelve bytes of UTF-8 (section 7.5.9).
///
/// A text longer than twelve bytes is spread over several packets, cut at
/// any byte, so one packet on its own need not be valid UTF-8. Collect the
/// pieces with an [`Assembler`](super::Assembler) and decode the whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FlexText {
    /// What the text is.
    pub kind: TextKind,
    /// Which piece of the whole text this is.
    pub part: Part,
    bytes: [u8; 12],
}

impl FlexText {
    /// The most bytes of text one packet holds.
    pub const MAX_BYTES: usize = 12;
    /// The most packets one flex data message may have, and so thirty-two
    /// times twelve bytes of text.
    pub const MAX_PACKETS: usize = 32;

    /// A packet holding `text`, padded with zeros. Fails if there are more
    /// than twelve bytes.
    pub fn new(kind: TextKind, part: Part, text: &[u8]) -> Result<Self, UmpError> {
        if text.len() > Self::MAX_BYTES {
            return Err(UmpError::Invalid("more than twelve bytes in a text packet"));
        }
        let mut bytes = [0u8; 12];
        bytes[..text.len()].copy_from_slice(text);
        Ok(Self { kind, part, bytes })
    }

    /// The text in this packet: the bytes up to the first zero, which
    /// marks the end of a text.
    ///
    /// Twelve zeros in a complete lyrics or ruby packet are a melisma: the
    /// previous syllable carries on (section 7.5.10.1). That reads as an
    /// empty text here.
    ///
    /// # Limits
    ///
    /// In lyrics and ruby text spread over several packets the
    /// specification gives a zero in a continue packet the meaning of a
    /// melisma as well (sections 7.5.10 and 7.5.12). This method does not
    /// know that case: it stops at the first zero of every packet, so
    /// such a zero is taken for the end of the text and any bytes behind
    /// it in the packet are left out. A reader that needs to tell the two
    /// apart has all twelve bytes in [`bytes`](Self::bytes).
    pub fn text(&self) -> &[u8] {
        until_nul(&self.bytes)
    }

    /// All twelve bytes, with the padding.
    pub fn bytes(&self) -> &[u8; 12] {
        &self.bytes
    }
}

/// Cuts a text into flex data packets of twelve bytes.
///
/// Fails if the text contains a zero byte, which the format uses as its
/// end marker, or is longer than the 384 bytes that the limit of 32
/// packets allows. Does not allocate.
///
/// Because every zero byte is refused, a lyric with a melisma marked by a
/// zero inside it (see [`FlexText::text`]) cannot be produced here. Its
/// packets have to be built one by one with [`FlexText::new`], which
/// takes any bytes.
///
/// ```
/// use manguebyte_midi::ump::{text_packets, FlexAddress, TextKind};
///
/// // The first example of figure 78.
/// let packets: Vec<_> = text_packets(0, FlexAddress::Group, TextKind::Copyright, "©2022 AMEI")
///     .unwrap()
///     .collect();
/// assert_eq!(packets.len(), 1);
/// assert_eq!(
///     packets[0].words(),
///     &[0xD010_0104, 0xC2A9_3230, 0x3232_2041, 0x4D45_4900]
/// );
/// ```
pub fn text_packets(
    group: u8,
    address: FlexAddress,
    kind: TextKind,
    text: &str,
) -> Result<impl Iterator<Item = Packet> + '_, UmpError> {
    let bytes = text.as_bytes();
    if bytes.contains(&0) {
        return Err(UmpError::Invalid("text contains a zero byte"));
    }
    if bytes.len() > FlexText::MAX_BYTES * FlexText::MAX_PACKETS {
        return Err(UmpError::Invalid(
            "text longer than a flex data message allows",
        ));
    }
    Ok(
        Pieces::new(bytes, FlexText::MAX_BYTES).map(move |(part, piece)| {
            let mut padded = [0u8; 12];
            padded[..piece.len()].copy_from_slice(piece);
            encode(&FlexData {
                group,
                address,
                message: FlexMessage::Text(FlexText {
                    kind,
                    part,
                    bytes: padded,
                }),
            })
        }),
    )
}

/// A four-bit two's complement field as a number, `-8..=7`.
fn signed4(bits: u8) -> i8 {
    ((bits << 4) as i8) >> 4
}

/// The four-bit two's complement form of a number, in the low bits.
fn unsigned4(value: i8) -> u8 {
    value as u8 & 0x0F
}

fn pack_alteration(alteration: Alteration) -> u8 {
    ((alteration.kind & 0x0F) << 4) | (alteration.degree & 0x0F)
}

fn unpack_alteration(byte: u8) -> Alteration {
    Alteration {
        kind: byte >> 4,
        degree: byte & 0x0F,
    }
}

pub(super) fn encode(flex: &FlexData) -> Packet {
    // Figure 69: message type, group, form, address, channel, status bank
    // and status, then twelve bytes of data.
    let (address_bits, channel) = match flex.address {
        FlexAddress::Channel(channel) => (0u8, channel & 0x0F),
        // The channel field is reserved when a group is addressed.
        FlexAddress::Group => (1, 0),
    };
    let mut bytes = [0u8; 16];
    let (form, bank, status) = match flex.message {
        FlexMessage::SetTempo {
            ten_nanoseconds_per_quarter,
        } => {
            bytes[4..8].copy_from_slice(&ten_nanoseconds_per_quarter.to_be_bytes());
            (Part::Complete, 0x00, 0x00)
        }
        FlexMessage::SetTimeSignature {
            numerator,
            denominator_power,
            thirty_seconds_per_quarter,
        } => {
            bytes[4..7].copy_from_slice(&[
                numerator,
                denominator_power,
                thirty_seconds_per_quarter,
            ]);
            (Part::Complete, 0x00, 0x01)
        }
        FlexMessage::SetMetronome(metronome) => {
            bytes[4] = metronome.clocks_per_primary_click;
            bytes[5..8].copy_from_slice(&metronome.bar_accents);
            bytes[8..10].copy_from_slice(&metronome.subdivision_clicks);
            (Part::Complete, 0x00, 0x02)
        }
        FlexMessage::SetKeySignature { sharps, tonic } => {
            bytes[4] = (unsigned4(sharps) << 4) | (tonic & 0x0F);
            (Part::Complete, 0x00, 0x05)
        }
        FlexMessage::SetChordName(chord) => {
            bytes[4] = (unsigned4(chord.tonic_accidentals) << 4) | (chord.tonic & 0x0F);
            bytes[5] = chord.chord_type;
            for (byte, alteration) in bytes[6..10].iter_mut().zip(chord.alterations) {
                *byte = pack_alteration(alteration);
            }
            bytes[12] = (unsigned4(chord.bass_accidentals) << 4) | (chord.bass & 0x0F);
            bytes[13] = chord.bass_chord_type;
            for (byte, alteration) in bytes[14..16].iter_mut().zip(chord.bass_alterations) {
                *byte = pack_alteration(alteration);
            }
            (Part::Complete, 0x00, 0x06)
        }
        FlexMessage::Text(text) => {
            bytes[4..].copy_from_slice(&text.bytes);
            let (bank, status) = text.kind.bank_and_status();
            (text.part, bank, status)
        }
    };
    let first = header(0xD, flex.group)
        | (form.bits() << 22)
        | (u32::from(address_bits) << 20)
        | (u32::from(channel) << 16)
        | (u32::from(bank) << 8)
        | u32::from(status);
    bytes[..4].copy_from_slice(&first.to_be_bytes());
    Packet::from_bytes16(bytes)
}

pub(super) fn decode(words: &[u32; 4]) -> Result<FlexData, UmpError> {
    let w0 = words[0];
    let part = Part::from_bits(w0 >> 22);
    let address = match (w0 >> 20) & 0x3 {
        0 => FlexAddress::Channel(nibble(w0, 16)),
        1 => FlexAddress::Group,
        _ => return Err(UmpError::Invalid("reserved flex data address")),
    };
    let (bank, status) = (octet(w0, 8), octet(w0, 0));
    let unknown = UmpError::UnknownStatus {
        message_type: 0xD,
        status: (u16::from(bank) << 8) | u16::from(status),
    };
    let bytes = bytes16(words);
    let message = match bank {
        0x00 => {
            let message = match status {
                0x00 => FlexMessage::SetTempo {
                    ten_nanoseconds_per_quarter: words[1],
                },
                0x01 => FlexMessage::SetTimeSignature {
                    numerator: bytes[4],
                    denominator_power: bytes[5],
                    thirty_seconds_per_quarter: bytes[6],
                },
                0x02 => FlexMessage::SetMetronome(Metronome {
                    clocks_per_primary_click: bytes[4],
                    bar_accents: [bytes[5], bytes[6], bytes[7]],
                    subdivision_clicks: [bytes[8], bytes[9]],
                }),
                0x05 => FlexMessage::SetKeySignature {
                    sharps: signed4(bytes[4] >> 4),
                    tonic: bytes[4] & 0x0F,
                },
                0x06 => FlexMessage::SetChordName(ChordName {
                    tonic_accidentals: signed4(bytes[4] >> 4),
                    tonic: bytes[4] & 0x0F,
                    chord_type: bytes[5],
                    alterations: [
                        unpack_alteration(bytes[6]),
                        unpack_alteration(bytes[7]),
                        unpack_alteration(bytes[8]),
                        unpack_alteration(bytes[9]),
                    ],
                    bass_accidentals: signed4(bytes[12] >> 4),
                    bass: bytes[12] & 0x0F,
                    bass_chord_type: bytes[13],
                    bass_alterations: [unpack_alteration(bytes[14]), unpack_alteration(bytes[15])],
                }),
                _ => return Err(unknown),
            };
            // These are defined as one packet each. A start or continue
            // packet with one of these statuses would be the beginning of
            // something this version of the format does not describe.
            if part != Part::Complete {
                return Err(UmpError::Invalid(
                    "a flex data setup message spread over several packets",
                ));
            }
            message
        }
        0x01 | 0x02 => {
            let kind = TextKind::from_bank_and_status(bank, status).ok_or(unknown)?;
            let mut text = [0u8; 12];
            text.copy_from_slice(&bytes[4..]);
            FlexMessage::Text(FlexText {
                kind,
                part,
                bytes: text,
            })
        }
        _ => return Err(unknown),
    };
    Ok(FlexData {
        group: nibble(w0, 24),
        address,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::super::{Assembler, Message};
    use super::*;

    fn packet(group: u8, address: FlexAddress, message: FlexMessage) -> Packet {
        Message::Flex(FlexData {
            group,
            address,
            message,
        })
        .to_packet()
    }

    fn round_trip(address: FlexAddress, message: FlexMessage) {
        let flex = FlexData {
            group: 13,
            address,
            message,
        };
        let packet = Message::Flex(flex).to_packet();
        assert_eq!(packet.word_count(), 4);
        assert_eq!(packet.message_type(), 0xD);
        assert_eq!(packet.group(), Some(13));
        assert_eq!(packet.message(), Ok(Message::Flex(flex)), "{packet:?}");
    }

    #[test]
    fn tempo_is_counted_in_units_of_ten_nanoseconds() {
        // Figure 70. A quarter note at 120 bpm lasts half a second, which
        // is 50 000 000 units.
        assert_eq!(
            packet(3, FlexAddress::Group, FlexMessage::tempo_from_bpm(120.0)).words(),
            &[0xD310_0000, 50_000_000, 0, 0]
        );
        assert_eq!(
            FlexMessage::tempo_from_bpm(60.0),
            FlexMessage::SetTempo {
                ten_nanoseconds_per_quarter: 100_000_000
            }
        );
        // "The slowest tempo supported is 1.39698 bpm" (section 7.5.3).
        let slowest = FlexMessage::SetTempo {
            ten_nanoseconds_per_quarter: u32::MAX,
        };
        assert!((slowest.tempo_bpm().unwrap() - 1.39698).abs() < 5e-6);
        assert_eq!(FlexMessage::tempo_from_bpm(1.0), slowest);
        assert_eq!(FlexMessage::tempo_from_bpm(0.0), slowest);
        assert_eq!(FlexMessage::tempo_from_bpm(-10.0), slowest);
        assert_eq!(FlexMessage::tempo_from_bpm(f64::NAN), slowest);
        assert_eq!(
            FlexMessage::tempo_from_bpm(f64::INFINITY),
            FlexMessage::SetTempo {
                ten_nanoseconds_per_quarter: 1
            }
        );
        assert_eq!(
            FlexMessage::SetTempo {
                ten_nanoseconds_per_quarter: 0
            }
            .tempo_bpm(),
            None
        );
        for bpm in [20.0, 59.94, 120.0, 187.5, 300.0] {
            let back = FlexMessage::tempo_from_bpm(bpm).tempo_bpm().unwrap();
            assert!((back - bpm).abs() < 1e-6, "{bpm} came back as {back}");
        }
    }

    #[test]
    fn time_and_key_signatures_match_their_figures() {
        // Figure 71: 6/8 with eight thirty-seconds to the quarter.
        assert_eq!(
            packet(
                0,
                FlexAddress::Group,
                FlexMessage::SetTimeSignature {
                    numerator: 6,
                    denominator_power: 3,
                    thirty_seconds_per_quarter: 8
                }
            )
            .words(),
            &[0xD010_0001, 0x0603_0800, 0, 0]
        );
        // Figure 74 with the rows of table 12: tonic D (4) with one sharp,
        // five sharps and four flats, here on channel 9.
        let key = |sharps| {
            packet(
                0,
                FlexAddress::Channel(9),
                FlexMessage::SetKeySignature { sharps, tonic: 4 },
            )
            .words()
            .to_vec()
        };
        assert_eq!(key(1), [0xD009_0005, 0x1400_0000, 0, 0]);
        assert_eq!(key(5), [0xD009_0005, 0x5400_0000, 0, 0]);
        assert_eq!(key(-4), [0xD009_0005, 0xC400_0000, 0, 0]);
        // -8 is "unknown or non-standard".
        assert_eq!(key(-8), [0xD009_0005, 0x8400_0000, 0, 0]);
    }

    #[test]
    fn metronome_examples_of_the_specification() {
        // Figure 73.
        let metronome = |clocks, bar_accents, subdivision_clicks| {
            let packet = packet(
                0,
                FlexAddress::Group,
                FlexMessage::SetMetronome(Metronome {
                    clocks_per_primary_click: clocks,
                    bar_accents,
                    subdivision_clicks,
                }),
            );
            assert_eq!(packet.words()[0], 0xD010_0002);
            assert_eq!(packet.words()[3], 0);
            [packet.words()[1], packet.words()[2]]
        };
        // 4/4 with a bar accent and no subdivisions.
        assert_eq!(metronome(24, [4, 0, 0], [0, 0]), [0x1804_0000, 0x0000_0000]);
        // 6/8 with a bar accent and eighth-note triplets.
        assert_eq!(metronome(36, [2, 0, 0], [3, 0]), [0x2402_0000, 0x0300_0000]);
        // 5/4 accented as 3 + 2, with eighth notes.
        assert_eq!(metronome(24, [3, 2, 0], [2, 0]), [0x1803_0200, 0x0200_0000]);
        // 3/4 with a bar accent, eighth notes and eighth-note triplets.
        assert_eq!(metronome(24, [3, 0, 0], [2, 3]), [0x1803_0000, 0x0203_0000]);
    }

    #[test]
    fn chord_name_examples_of_the_specification() {
        // Figure 76.
        let chord = |chord| {
            let packet = packet(0, FlexAddress::Group, FlexMessage::SetChordName(chord));
            assert_eq!(packet.words()[0], 0xD010_0006);
            [packet.words()[1], packet.words()[2], packet.words()[3]]
        };
        // B flat minor.
        assert_eq!(
            chord(ChordName::new(2, -1, chord_type::MINOR)),
            [0xF207_0000, 0, 0x8000_0000]
        );
        // D major over F sharp.
        let mut d_over_f_sharp = ChordName::new(4, 0, chord_type::MAJOR);
        d_over_f_sharp.bass_accidentals = 1;
        d_over_f_sharp.bass = 6;
        assert_eq!(chord(d_over_f_sharp), [0x0401_0000, 0, 0x1600_0000]);
        // C major seventh with a raised eleventh.
        let mut c_maj7_sharp11 = ChordName::new(3, 0, chord_type::MAJOR_7TH);
        c_maj7_sharp11.alterations[0] = Alteration {
            kind: Alteration::RAISE,
            degree: 11,
        };
        assert_eq!(chord(c_maj7_sharp11), [0x0303_3B00, 0, 0x8000_0000]);
    }

    #[test]
    fn chord_names_keep_every_field() {
        // Figure 75, every field given a different value.
        let chord = ChordName {
            tonic_accidentals: 2,
            tonic: 7,
            chord_type: chord_type::SEVEN_SUSPENDED_4TH,
            alterations: [
                Alteration { kind: 1, degree: 2 },
                Alteration { kind: 3, degree: 4 },
                Alteration { kind: 2, degree: 9 },
                Alteration {
                    kind: 4,
                    degree: 13,
                },
            ],
            bass_accidentals: -2,
            bass: 1,
            bass_chord_type: chord_type::DIMINISHED,
            bass_alterations: [
                Alteration { kind: 1, degree: 6 },
                Alteration { kind: 4, degree: 5 },
            ],
        };
        assert_eq!(
            packet(5, FlexAddress::Channel(2), FlexMessage::SetChordName(chord)).words(),
            &[0xD502_0006, 0x271B_1234, 0x294D_0000, 0xE113_1645]
        );
        round_trip(FlexAddress::Channel(2), FlexMessage::SetChordName(chord));
    }

    #[test]
    fn text_examples_of_the_specification() {
        // Figure 78, second example: forty-two bytes in four packets.
        let text = "Copyright 2022 MIDI Association Publishing";
        let packets: Vec<Packet> = text_packets(0, FlexAddress::Group, TextKind::Copyright, text)
            .unwrap()
            .collect();
        let expected: [[u8; 16]; 4] = [
            *b"\xD0\x50\x01\x04Copyright 20",
            *b"\xD0\x90\x01\x0422 MIDI Asso",
            *b"\xD0\x90\x01\x04ciation Publ",
            *b"\xD0\xD0\x01\x04ishing\0\0\0\0\0\0",
        ];
        assert_eq!(packets.len(), 4);
        for (packet, bytes) in packets.iter().zip(expected) {
            let words: Vec<u8> = packet
                .words()
                .iter()
                .flat_map(|word| word.to_be_bytes())
                .collect();
            assert_eq!(words, bytes);
        }

        // And back again.
        let mut assembler = Assembler::new(FlexText::MAX_BYTES * FlexText::MAX_PACKETS);
        let mut whole = None;
        for packet in packets {
            let Ok(Message::Flex(FlexData {
                message: FlexMessage::Text(piece),
                ..
            })) = packet.message()
            else {
                panic!("not a text packet: {packet:?}");
            };
            assert_eq!(piece.kind, TextKind::Copyright);
            if let Some(bytes) = assembler.push(piece.part, piece.text()) {
                whole = Some(String::from_utf8(bytes.to_vec()).unwrap());
            }
        }
        assert_eq!(whole.as_deref(), Some(text));
    }

    #[test]
    fn text_is_cut_at_twelve_bytes_and_limited_to_thirty_two_packets() {
        let parts = |text: &str| -> Vec<(Part, usize)> {
            text_packets(1, FlexAddress::Channel(3), TextKind::Lyrics, text)
                .unwrap()
                .map(|packet| match packet.message() {
                    Ok(Message::Flex(FlexData {
                        group: 1,
                        address: FlexAddress::Channel(3),
                        message: FlexMessage::Text(text),
                    })) => (text.part, text.text().len()),
                    other => panic!("unexpected {other:?}"),
                })
                .collect()
        };
        // An empty text is one packet of zeros, which for lyrics is a
        // melisma.
        assert_eq!(parts(""), [(Part::Complete, 0)]);
        assert_eq!(parts("la"), [(Part::Complete, 2)]);
        assert_eq!(parts("twelve bytes"), [(Part::Complete, 12)]);
        assert_eq!(parts("thirteen byte"), [(Part::Start, 12), (Part::End, 1)]);
        assert_eq!(parts(&"x".repeat(24)), [(Part::Start, 12), (Part::End, 12)]);
        let longest = "y".repeat(384);
        assert_eq!(parts(&longest).len(), 32);
        assert!(text_packets(0, FlexAddress::Group, TextKind::Lyrics, &"y".repeat(385)).is_err());
        assert!(text_packets(0, FlexAddress::Group, TextKind::Lyrics, "a\0b").is_err());
        assert!(FlexText::new(TextKind::Lyrics, Part::Complete, &[b'a'; 13]).is_err());
    }

    #[test]
    fn a_zero_inside_a_packet_ends_its_text_but_stays_in_its_bytes() {
        // The documented limit of `text`: a zero that stands for a melisma
        // inside a lyric of several packets reads as the end of the text.
        // The packet itself carries all twelve bytes through unchanged, so
        // a reader that knows the case can find it.
        let piece = FlexText::new(TextKind::Lyrics, Part::Continue, b"la\0la").unwrap();
        assert_eq!(piece.text(), b"la");
        assert_eq!(piece.bytes(), b"la\0la\0\0\0\0\0\0\0");
        round_trip(FlexAddress::Channel(2), FlexMessage::Text(piece));
        let Ok(Message::Flex(FlexData {
            message: FlexMessage::Text(read),
            ..
        })) = packet(0, FlexAddress::Group, FlexMessage::Text(piece)).message()
        else {
            panic!("not a text packet");
        };
        assert_eq!(read.bytes(), piece.bytes());
    }

    #[test]
    fn every_flex_message_round_trips() {
        for address in [
            FlexAddress::Group,
            FlexAddress::Channel(0),
            FlexAddress::Channel(15),
        ] {
            round_trip(
                address,
                FlexMessage::SetTempo {
                    ten_nanoseconds_per_quarter: 0xFFFF_FFFF,
                },
            );
            round_trip(
                address,
                FlexMessage::SetTimeSignature {
                    numerator: 255,
                    denominator_power: 0,
                    thirty_seconds_per_quarter: 8,
                },
            );
            round_trip(
                address,
                FlexMessage::SetMetronome(Metronome {
                    clocks_per_primary_click: 255,
                    bar_accents: [1, 2, 3],
                    subdivision_clicks: [4, 5],
                }),
            );
            for sharps in -8..=7 {
                for tonic in 0..16 {
                    round_trip(address, FlexMessage::SetKeySignature { sharps, tonic });
                }
            }
            round_trip(
                address,
                FlexMessage::SetChordName(ChordName::new(1, -2, chord_type::POWER)),
            );
            for (kind, _, _) in TextKind::ALL {
                for part in [Part::Complete, Part::Start, Part::Continue, Part::End] {
                    let text = FlexText::new(kind, part, "añb".as_bytes()).unwrap();
                    assert_eq!(text.text(), "añb".as_bytes());
                    assert_eq!(&text.bytes()[4..], &[0; 8]);
                    round_trip(address, FlexMessage::Text(text));
                }
            }
        }
        for (kind, bank, status) in TextKind::ALL {
            assert_eq!(kind.bank_and_status(), (bank, status));
            assert_eq!(TextKind::from_bank_and_status(bank, status), Some(kind));
        }
        assert_eq!(TextKind::from_bank_and_status(1, 0x0D), None);
        assert_eq!(TextKind::from_bank_and_status(2, 0x05), None);
        assert_eq!(TextKind::from_bank_and_status(0, 0x00), None);
    }

    #[test]
    fn four_bit_signed_fields_wrap_like_twos_complement() {
        for value in -8i8..=7 {
            assert_eq!(signed4(unsigned4(value)), value);
        }
        // Table 13.
        assert_eq!(unsigned4(2), 0x2);
        assert_eq!(unsigned4(1), 0x1);
        assert_eq!(unsigned4(0), 0x0);
        assert_eq!(unsigned4(-1), 0xF);
        assert_eq!(unsigned4(-2), 0xE);
        assert_eq!(unsigned4(-8), 0x8);
    }

    #[test]
    fn malformed_flex_data_is_rejected() {
        let read = |w0: u32| Packet::from_words(&[w0, 1, 2, 3]).unwrap().message();
        // Reserved address values 2 and 3.
        assert_eq!(
            read(0xD020_0000),
            Err(UmpError::Invalid("reserved flex data address"))
        );
        assert_eq!(
            read(0xD030_0000),
            Err(UmpError::Invalid("reserved flex data address"))
        );
        // Setup messages are one packet; start, continue and end are not
        // defined for them.
        for form in 1..4u32 {
            for status in [0x00u32, 0x01, 0x02, 0x05, 0x06] {
                assert!(matches!(
                    read(0xD010_0000 | (form << 22) | status),
                    Err(UmpError::Invalid(_))
                ));
            }
        }
        // Statuses and status banks that are not defined.
        for (bank, status) in [
            (0u32, 3u32),
            (0, 4),
            (0, 7),
            (1, 0x0D),
            (2, 5),
            (3, 0),
            (0xFF, 0xFF),
        ] {
            assert_eq!(
                read(0xD010_0000 | (bank << 8) | status),
                Err(UmpError::UnknownStatus {
                    message_type: 0xD,
                    status: ((bank << 8) | status) as u16
                })
            );
        }
        // The channel field is ignored when the group is addressed.
        assert_eq!(
            read(0xD01F_0000),
            Ok(Message::Flex(FlexData {
                group: 0,
                address: FlexAddress::Group,
                message: FlexMessage::SetTempo {
                    ten_nanoseconds_per_quarter: 1
                }
            }))
        );
    }
}
