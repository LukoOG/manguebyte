//! Standard MIDI Files (`.mid`).
//!
//! Reads and writes formats 0, 1 and 2 as described in "Standard MIDI Files
//! 1.0" (the MIDI Association, RP-001).
//!
//! ```
//! use manguebyte_midi::MidiMessage;
//! use manguebyte_midi::file::{MetaEvent, MidiFile, Track};
//!
//! let mut track = Track::new();
//! track.push_meta(0, MetaEvent::Tempo(500_000)); // 120 beats per minute
//! track.push_midi(0, MidiMessage::NoteOn { channel: 0, note: 60, velocity: 100 });
//! track.push_midi(480, MidiMessage::NoteOff { channel: 0, note: 60, velocity: 0 });
//!
//! let file = MidiFile::single_track(480, track);
//! let bytes = file.to_bytes();
//!
//! let read_back = MidiFile::from_bytes(&bytes).unwrap();
//! assert_eq!(read_back.tracks[0].end_tick(), 480);
//! // One quarter note at 120 bpm lasts half a second.
//! assert!((read_back.tick_to_seconds(480).unwrap() - 0.5).abs() < 1e-12);
//! ```

use std::fmt;

use crate::MidiMessage;

/// How the ticks that events are timed in relate to musical or clock time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Division {
    /// Ticks per quarter note. How long a tick lasts depends on the tempo.
    TicksPerQuarter(u16),
    /// Ticks are fractions of a video frame and do not depend on tempo.
    Smpte {
        /// Frames per second: 24, 25, 29 (meaning 29.97) or 30.
        frames_per_second: u8,
        /// Ticks in each frame.
        ticks_per_frame: u8,
    },
}

/// An event that carries information about the music rather than a message
/// for an instrument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetaEvent {
    /// The length of a quarter note, in microseconds. 500 000 is 120 bpm.
    Tempo(u32),
    /// The time signature.
    TimeSignature {
        /// Beats in a bar.
        numerator: u8,
        /// The note value of a beat as a power of two: 2 means a quarter
        /// note, 3 an eighth.
        denominator_power: u8,
        /// MIDI clocks between metronome clicks.
        clocks_per_click: u8,
        /// Thirty-second notes in 24 MIDI clocks, normally 8.
        thirty_seconds_per_quarter: u8,
    },
    /// The key signature.
    KeySignature {
        /// Number of sharps, or of flats when negative.
        sharps: i8,
        /// Whether the key is minor.
        minor: bool,
    },
    /// The name of the track, or of the whole piece on the first track.
    TrackName(String),
    /// The name of the instrument the track is meant for.
    InstrumentName(String),
    /// A point of interest, such as "Verse 2".
    Marker(String),
    /// A syllable of lyrics.
    Lyric(String),
    /// Free text.
    Text(String),
    /// The end of the track. Written automatically; kept when reading so
    /// that a track's length is not lost.
    EndOfTrack,
    /// Any other meta event, kept as it was found.
    Other {
        /// The meta event type byte.
        kind: u8,
        /// The event's data.
        data: Vec<u8>,
    },
}

/// What happens at one moment in a track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackEventKind {
    /// A channel voice message.
    Midi(MidiMessage),
    /// The payload of a system exclusive message, without the `0xF0` that
    /// starts it. A complete message ends with `0xF7`.
    SysEx(Vec<u8>),
    /// A meta event.
    Meta(MetaEvent),
}

/// An event and when it happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackEvent {
    /// Time in ticks from the start of the track.
    pub tick: u64,
    /// What happens.
    pub kind: TrackEventKind,
}

/// A sequence of events in time order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Track {
    events: Vec<TrackEvent>,
}

impl Track {
    /// An empty track.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an event, keeping the track in time order. Events at the same
    /// tick stay in the order they were added.
    pub fn push(&mut self, tick: u64, kind: TrackEventKind) {
        let index = self.events.partition_point(|event| event.tick <= tick);
        self.events.insert(index, TrackEvent { tick, kind });
    }

    /// Adds a channel voice message.
    pub fn push_midi(&mut self, tick: u64, message: MidiMessage) {
        self.push(tick, TrackEventKind::Midi(message));
    }

    /// Adds a meta event.
    pub fn push_meta(&mut self, tick: u64, event: MetaEvent) {
        self.push(tick, TrackEventKind::Meta(event));
    }

    /// The events, in time order.
    pub fn events(&self) -> &[TrackEvent] {
        &self.events
    }

    /// How many events the track holds.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether the track holds no events.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// The tick of the last event, or zero for an empty track.
    pub fn end_tick(&self) -> u64 {
        self.events.last().map_or(0, |event| event.tick)
    }

    /// The track's name, if it has one.
    pub fn name(&self) -> Option<&str> {
        self.events.iter().find_map(|event| match &event.kind {
            TrackEventKind::Meta(MetaEvent::TrackName(name)) => Some(name.as_str()),
            _ => None,
        })
    }
}

/// Why bytes could not be read as a MIDI file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidiFileError {
    /// The data does not start with a MIDI file header.
    NotMidiFile,
    /// The data ends in the middle of something.
    Truncated,
    /// The header or an event breaks the format's rules.
    Malformed(&'static str),
}

impl fmt::Display for MidiFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotMidiFile => f.write_str("not a standard MIDI file"),
            Self::Truncated => f.write_str("MIDI file ends unexpectedly"),
            Self::Malformed(reason) => write!(f, "malformed MIDI file: {reason}"),
        }
    }
}

impl std::error::Error for MidiFileError {}

/// The contents of a standard MIDI file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiFile {
    /// `0` for a single track, `1` for tracks played together, `2` for
    /// independent patterns.
    pub format: u16,
    /// What a tick means.
    pub division: Division,
    /// The tracks.
    pub tracks: Vec<Track>,
}

/// Longest delta time or length the format can express.
const MAX_VARIABLE_LENGTH: u64 = 0x0FFF_FFFF;

impl MidiFile {
    /// A format 0 file with one track and `ticks_per_quarter` resolution.
    pub fn single_track(ticks_per_quarter: u16, track: Track) -> Self {
        Self {
            format: 0,
            division: Division::TicksPerQuarter(ticks_per_quarter),
            tracks: vec![track],
        }
    }

    /// A format 1 file: tracks that play together, the first conventionally
    /// holding tempo and time signature.
    pub fn multi_track(ticks_per_quarter: u16, tracks: Vec<Track>) -> Self {
        Self {
            format: 1,
            division: Division::TicksPerQuarter(ticks_per_quarter),
            tracks,
        }
    }

    /// Reads a MIDI file from memory.
    ///
    /// Unknown chunks are skipped and unknown meta events are kept as
    /// [`MetaEvent::Other`], as the specification asks. Data that is not a
    /// MIDI file or is cut short produces an error, never a panic.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, MidiFileError> {
        let mut reader = Reader { bytes };
        if reader.bytes.len() < 4 || &reader.bytes[..4] != b"MThd" {
            return Err(MidiFileError::NotMidiFile);
        }
        reader.take(4)?;
        let header_len = reader.u32()? as usize;
        if header_len < 6 {
            return Err(MidiFileError::Malformed("header shorter than six bytes"));
        }
        let mut header = Reader {
            bytes: reader.take(header_len)?,
        };
        let format = header.u16()?;
        let track_count = header.u16()?;
        let raw_division = header.u16()?;
        if format > 2 {
            return Err(MidiFileError::Malformed("unknown file format"));
        }
        let division = if raw_division & 0x8000 == 0 {
            if raw_division == 0 {
                return Err(MidiFileError::Malformed("zero ticks per quarter note"));
            }
            Division::TicksPerQuarter(raw_division)
        } else {
            Division::Smpte {
                frames_per_second: ((raw_division >> 8) as u8 as i8).unsigned_abs(),
                ticks_per_frame: raw_division as u8,
            }
        };

        let mut tracks = Vec::new();
        while !reader.bytes.is_empty() && tracks.len() < usize::from(track_count) {
            let kind = reader.take(4)?;
            let len = reader.u32()? as usize;
            let body = reader.take(len)?;
            if kind == b"MTrk" {
                tracks.push(read_track(body)?);
            }
        }
        Ok(Self {
            format,
            division,
            tracks,
        })
    }

    /// Writes the file to memory.
    ///
    /// Every track is ended with an end-of-track event at its last tick,
    /// whether or not it already has one.
    ///
    /// # Panics
    ///
    /// Panics if two consecutive events are further apart than the format
    /// can express (about 268 million ticks), or if there are more than
    /// 65 535 tracks.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"MThd");
        out.extend_from_slice(&6u32.to_be_bytes());
        out.extend_from_slice(&self.format.to_be_bytes());
        let track_count = u16::try_from(self.tracks.len()).expect("too many tracks");
        out.extend_from_slice(&track_count.to_be_bytes());
        let division = match self.division {
            Division::TicksPerQuarter(ticks) => ticks & 0x7FFF,
            Division::Smpte {
                frames_per_second,
                ticks_per_frame,
            } => {
                let frames = (frames_per_second as i8).wrapping_neg() as u8;
                (u16::from(frames) << 8) | u16::from(ticks_per_frame)
            }
        };
        out.extend_from_slice(&division.to_be_bytes());

        for track in &self.tracks {
            let body = write_track(track);
            out.extend_from_slice(b"MTrk");
            out.extend_from_slice(&(body.len() as u32).to_be_bytes());
            out.extend_from_slice(&body);
        }
        out
    }

    /// The time in seconds of a tick, following every tempo change in the
    /// file.
    ///
    /// Tempo events are taken from all tracks, which is right for formats 0
    /// and 1. Before the first tempo event the tempo is 120 beats per minute,
    /// as the specification says. Returns `None` for format 2, whose tracks
    /// each keep their own time.
    pub fn tick_to_seconds(&self, tick: u64) -> Option<f64> {
        let ticks_per_quarter = match self.division {
            Division::Smpte {
                frames_per_second,
                ticks_per_frame,
            } => {
                let frames = if frames_per_second == 29 {
                    30_000.0 / 1_001.0
                } else {
                    f64::from(frames_per_second)
                };
                return Some(tick as f64 / (frames * f64::from(ticks_per_frame)));
            }
            Division::TicksPerQuarter(ticks) => f64::from(ticks),
        };
        if self.format == 2 {
            return None;
        }

        let mut tempos: Vec<(u64, u32)> = self
            .tracks
            .iter()
            .flat_map(|track| track.events())
            .filter_map(|event| match event.kind {
                TrackEventKind::Meta(MetaEvent::Tempo(tempo)) => Some((event.tick, tempo)),
                _ => None,
            })
            .collect();
        tempos.sort_by_key(|&(at, _)| at);

        let mut seconds = 0.0;
        let mut position = 0u64;
        let mut microseconds_per_quarter = 500_000.0;
        for (at, tempo) in tempos {
            if at >= tick {
                break;
            }
            seconds += (at - position) as f64 * microseconds_per_quarter * 1e-6 / ticks_per_quarter;
            position = at;
            microseconds_per_quarter = f64::from(tempo);
        }
        Some(
            seconds
                + (tick - position) as f64 * microseconds_per_quarter * 1e-6 / ticks_per_quarter,
        )
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], MidiFileError> {
        if count > self.bytes.len() {
            return Err(MidiFileError::Truncated);
        }
        let (head, tail) = self.bytes.split_at(count);
        self.bytes = tail;
        Ok(head)
    }

    fn u8(&mut self) -> Result<u8, MidiFileError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, MidiFileError> {
        let bytes = self.take(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Result<u32, MidiFileError> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// A variable-length quantity: seven bits per byte, most significant
    /// first, with the top bit set on every byte but the last.
    fn variable_length(&mut self) -> Result<u32, MidiFileError> {
        let mut value = 0u32;
        for _ in 0..4 {
            let byte = self.u8()?;
            value = (value << 7) | u32::from(byte & 0x7F);
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(MidiFileError::Malformed(
            "variable-length quantity longer than four bytes",
        ))
    }
}

fn read_track(body: &[u8]) -> Result<Track, MidiFileError> {
    let mut reader = Reader { bytes: body };
    let mut events = Vec::new();
    let mut tick = 0u64;
    let mut running_status = 0u8;

    while !reader.bytes.is_empty() {
        tick += u64::from(reader.variable_length()?);
        let first = reader.u8()?;
        let kind = match first {
            0xFF => {
                let meta_kind = reader.u8()?;
                let len = reader.variable_length()? as usize;
                TrackEventKind::Meta(read_meta(meta_kind, reader.take(len)?)?)
            }
            0xF0 | 0xF7 => {
                let len = reader.variable_length()? as usize;
                running_status = 0;
                TrackEventKind::SysEx(reader.take(len)?.to_vec())
            }
            0xF1..=0xFE => {
                return Err(MidiFileError::Malformed(
                    "system message other than sysex inside a track",
                ));
            }
            _ => {
                // A channel message, possibly with its status left out.
                let (status, first_data) = if first >= 0x80 {
                    running_status = first;
                    (first, reader.u8()?)
                } else if running_status != 0 {
                    (running_status, first)
                } else {
                    return Err(MidiFileError::Malformed("data byte with no status"));
                };
                let second_data = match status & 0xF0 {
                    0xC0 | 0xD0 => 0,
                    _ => reader.u8()?,
                };
                let message = MidiMessage::from_bytes(&[status, first_data, second_data])
                    .ok_or(MidiFileError::Malformed("invalid channel message"))?;
                TrackEventKind::Midi(message)
            }
        };
        let is_end = kind == TrackEventKind::Meta(MetaEvent::EndOfTrack);
        events.push(TrackEvent { tick, kind });
        if is_end {
            break;
        }
    }
    Ok(Track { events })
}

fn read_meta(kind: u8, data: &[u8]) -> Result<MetaEvent, MidiFileError> {
    let text = || String::from_utf8_lossy(data).into_owned();
    Ok(match (kind, data) {
        (0x01, _) => MetaEvent::Text(text()),
        (0x03, _) => MetaEvent::TrackName(text()),
        (0x04, _) => MetaEvent::InstrumentName(text()),
        (0x05, _) => MetaEvent::Lyric(text()),
        (0x06, _) => MetaEvent::Marker(text()),
        (0x2F, _) => MetaEvent::EndOfTrack,
        (0x51, &[a, b, c]) => MetaEvent::Tempo(u32::from_be_bytes([0, a, b, c])),
        (
            0x58,
            &[
                numerator,
                denominator_power,
                clocks_per_click,
                thirty_seconds_per_quarter,
            ],
        ) => MetaEvent::TimeSignature {
            numerator,
            denominator_power,
            clocks_per_click,
            thirty_seconds_per_quarter,
        },
        (0x59, &[sharps, minor]) => MetaEvent::KeySignature {
            sharps: sharps as i8,
            minor: minor != 0,
        },
        (0x51 | 0x58 | 0x59, _) => {
            return Err(MidiFileError::Malformed("meta event of the wrong length"));
        }
        _ => MetaEvent::Other {
            kind,
            data: data.to_vec(),
        },
    })
}

fn write_variable_length(out: &mut Vec<u8>, value: u64) {
    assert!(
        value <= MAX_VARIABLE_LENGTH,
        "{value} does not fit in a MIDI file variable-length quantity"
    );
    let mut started = false;
    for shift in [21, 14, 7] {
        let septet = ((value >> shift) & 0x7F) as u8;
        if septet != 0 || started {
            out.push(septet | 0x80);
            started = true;
        }
    }
    out.push((value & 0x7F) as u8);
}

fn write_meta(out: &mut Vec<u8>, kind: u8, data: &[u8]) {
    out.extend_from_slice(&[0xFF, kind]);
    write_variable_length(out, data.len() as u64);
    out.extend_from_slice(data);
}

fn write_track(track: &Track) -> Vec<u8> {
    let mut out = Vec::new();
    let mut previous_tick = 0u64;

    for event in track.events() {
        if event.kind == TrackEventKind::Meta(MetaEvent::EndOfTrack) {
            // Written once, below, so it is always last.
            continue;
        }
        write_variable_length(&mut out, event.tick - previous_tick);
        previous_tick = event.tick;
        match &event.kind {
            TrackEventKind::Midi(message) => out.extend_from_slice(message.to_bytes().as_slice()),
            TrackEventKind::SysEx(data) => {
                out.push(0xF0);
                write_variable_length(&mut out, data.len() as u64);
                out.extend_from_slice(data);
            }
            TrackEventKind::Meta(meta) => match meta {
                MetaEvent::Text(text) => write_meta(&mut out, 0x01, text.as_bytes()),
                MetaEvent::TrackName(text) => write_meta(&mut out, 0x03, text.as_bytes()),
                MetaEvent::InstrumentName(text) => write_meta(&mut out, 0x04, text.as_bytes()),
                MetaEvent::Lyric(text) => write_meta(&mut out, 0x05, text.as_bytes()),
                MetaEvent::Marker(text) => write_meta(&mut out, 0x06, text.as_bytes()),
                MetaEvent::Tempo(tempo) => {
                    write_meta(
                        &mut out,
                        0x51,
                        &(*tempo).min(0x00FF_FFFF).to_be_bytes()[1..],
                    );
                }
                MetaEvent::TimeSignature {
                    numerator,
                    denominator_power,
                    clocks_per_click,
                    thirty_seconds_per_quarter,
                } => write_meta(
                    &mut out,
                    0x58,
                    &[
                        *numerator,
                        *denominator_power,
                        *clocks_per_click,
                        *thirty_seconds_per_quarter,
                    ],
                ),
                MetaEvent::KeySignature { sharps, minor } => {
                    write_meta(&mut out, 0x59, &[*sharps as u8, u8::from(*minor)]);
                }
                MetaEvent::Other { kind, data } => write_meta(&mut out, *kind, data),
                MetaEvent::EndOfTrack => {}
            },
        }
    }

    write_variable_length(&mut out, track.end_tick() - previous_tick);
    write_meta(&mut out, 0x2F, &[]);
    out
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

    fn note_off(note: u8) -> MidiMessage {
        MidiMessage::NoteOff {
            channel: 0,
            note,
            velocity: 0,
        }
    }

    /// A file written by hand from the specification's own example layout:
    /// format 0, 96 ticks per quarter, using running status.
    const HAND_WRITTEN: &[u8] = &[
        b'M', b'T', b'h', b'd', 0, 0, 0, 6, 0, 0, 0, 1, 0, 96, // header
        b'M', b'T', b'r', b'k', 0, 0, 0, 34, // track chunk, 34 bytes
        0x00, 0xFF, 0x58, 0x04, 0x04, 0x02, 0x18, 0x08, // 4/4
        0x00, 0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20, // tempo 500000
        0x00, 0x90, 60, 100, // note on
        0x60, 64, 100, // 96 ticks later, running status
        0x81, 0x40, 0x80, 60, 0, // 192 ticks later, note off
        0x00, 64, 0, // running status again
        0x00, 0xFF, 0x2F, 0x00, // end of track
    ];

    #[test]
    fn reads_a_hand_written_file_with_running_status() {
        let file = MidiFile::from_bytes(HAND_WRITTEN).unwrap();
        assert_eq!(file.format, 0);
        assert_eq!(file.division, Division::TicksPerQuarter(96));
        assert_eq!(file.tracks.len(), 1);

        let events = file.tracks[0].events();
        let kinds: Vec<(u64, &TrackEventKind)> = events.iter().map(|e| (e.tick, &e.kind)).collect();
        assert_eq!(
            kinds,
            [
                (
                    0,
                    &TrackEventKind::Meta(MetaEvent::TimeSignature {
                        numerator: 4,
                        denominator_power: 2,
                        clocks_per_click: 24,
                        thirty_seconds_per_quarter: 8,
                    })
                ),
                (0, &TrackEventKind::Meta(MetaEvent::Tempo(500_000))),
                (0, &TrackEventKind::Midi(note_on(60))),
                (96, &TrackEventKind::Midi(note_on(64))),
                (288, &TrackEventKind::Midi(note_off(60))),
                (288, &TrackEventKind::Midi(note_off(64))),
                (288, &TrackEventKind::Meta(MetaEvent::EndOfTrack)),
            ]
        );
    }

    #[test]
    fn round_trips_every_kind_of_event() {
        let mut conductor = Track::new();
        conductor.push_meta(0, MetaEvent::TrackName(String::from("Maracatu atômico")));
        conductor.push_meta(0, MetaEvent::Tempo(600_000));
        conductor.push_meta(
            0,
            MetaEvent::KeySignature {
                sharps: -3,
                minor: true,
            },
        );
        conductor.push_meta(1_920, MetaEvent::Marker(String::from("Verse")));
        conductor.push_meta(
            3_000,
            MetaEvent::Other {
                kind: 0x7F,
                data: vec![1, 2, 3],
            },
        );
        conductor.push_meta(4_000_000, MetaEvent::EndOfTrack);

        let mut part = Track::new();
        part.push_meta(0, MetaEvent::InstrumentName(String::from("Alfaia")));
        part.push(0, TrackEventKind::SysEx(vec![0x7E, 0x7F, 0x09, 0x01, 0xF7]));
        part.push_midi(
            0,
            MidiMessage::ProgramChange {
                channel: 9,
                program: 5,
            },
        );
        part.push_midi(
            10,
            MidiMessage::PitchBend {
                channel: 9,
                value: -8_192,
            },
        );
        part.push_midi(
            127,
            MidiMessage::ControlChange {
                channel: 9,
                controller: 7,
                value: 127,
            },
        );
        part.push_midi(128, note_on(36));
        part.push_midi(16_384, note_off(36));
        part.push_meta(16_384, MetaEvent::Lyric(String::from("la")));
        part.push_meta(20_000, MetaEvent::EndOfTrack);

        let file = MidiFile::multi_track(960, vec![conductor, part]);
        let read_back = MidiFile::from_bytes(&file.to_bytes()).unwrap();
        assert_eq!(read_back, file);
        assert_eq!(read_back.tracks[0].name(), Some("Maracatu atômico"));
        assert_eq!(read_back.tracks[1].end_tick(), 20_000);
    }

    #[test]
    fn writing_always_ends_a_track_exactly_once() {
        let mut track = Track::new();
        track.push_midi(0, note_on(60));
        track.push_midi(100, note_off(60));
        let bytes = MidiFile::single_track(480, track).to_bytes();
        assert_eq!(&bytes[bytes.len() - 4..], &[0x00, 0xFF, 0x2F, 0x00]);

        let read = MidiFile::from_bytes(&bytes).unwrap();
        let ends = read.tracks[0]
            .events()
            .iter()
            .filter(|e| e.kind == TrackEventKind::Meta(MetaEvent::EndOfTrack))
            .count();
        assert_eq!(ends, 1);
        assert_eq!(read.tracks[0].end_tick(), 100);
    }

    #[test]
    fn track_keeps_time_order_and_insertion_order_within_a_tick() {
        let mut track = Track::new();
        track.push_midi(50, note_on(1));
        track.push_midi(10, note_on(2));
        track.push_midi(50, note_on(3));
        track.push_midi(10, note_on(4));
        let notes: Vec<_> = track
            .events()
            .iter()
            .map(|event| match event.kind {
                TrackEventKind::Midi(MidiMessage::NoteOn { note, .. }) => note,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(notes, [2, 4, 1, 3]);
        assert_eq!(track.len(), 4);
        assert!(!track.is_empty());
    }

    #[test]
    fn variable_length_quantities_match_the_specification_table() {
        for (value, expected) in [
            (0x00u64, &[0x00][..]),
            (0x40, &[0x40]),
            (0x7F, &[0x7F]),
            (0x80, &[0x81, 0x00]),
            (0x2000, &[0xC0, 0x00]),
            (0x3FFF, &[0xFF, 0x7F]),
            (0x4000, &[0x81, 0x80, 0x00]),
            (0x001F_FFFF, &[0xFF, 0xFF, 0x7F]),
            (0x0020_0000, &[0x81, 0x80, 0x80, 0x00]),
            (0x0FFF_FFFF, &[0xFF, 0xFF, 0xFF, 0x7F]),
        ] {
            let mut written = Vec::new();
            write_variable_length(&mut written, value);
            assert_eq!(written, expected, "{value:#x}");
            let mut reader = Reader { bytes: &written };
            assert_eq!(u64::from(reader.variable_length().unwrap()), value);
        }
    }

    #[test]
    fn tempo_changes_are_followed_when_converting_to_seconds() {
        let mut track = Track::new();
        // 120 bpm for two beats, then 60 bpm.
        track.push_meta(960, MetaEvent::Tempo(1_000_000));
        let file = MidiFile::single_track(480, track);
        assert_eq!(file.tick_to_seconds(0), Some(0.0));
        assert!((file.tick_to_seconds(480).unwrap() - 0.5).abs() < 1e-12);
        assert!((file.tick_to_seconds(960).unwrap() - 1.0).abs() < 1e-12);
        assert!((file.tick_to_seconds(1_440).unwrap() - 2.0).abs() < 1e-12);

        let smpte = MidiFile {
            format: 0,
            division: Division::Smpte {
                frames_per_second: 25,
                ticks_per_frame: 40,
            },
            tracks: vec![],
        };
        assert!((smpte.tick_to_seconds(1_000).unwrap() - 1.0).abs() < 1e-12);
        let read_back = MidiFile::from_bytes(&smpte.to_bytes()).unwrap();
        assert_eq!(read_back.division, smpte.division);
    }

    #[test]
    fn skips_unknown_chunks() {
        let mut bytes = HAND_WRITTEN[..14].to_vec();
        bytes.extend_from_slice(b"XFIH\0\0\0\x03abc");
        bytes.extend_from_slice(&HAND_WRITTEN[14..]);
        let file = MidiFile::from_bytes(&bytes).unwrap();
        assert_eq!(file.tracks.len(), 1);
        assert_eq!(file.tracks[0].len(), 7);
    }

    #[test]
    fn bad_input_is_an_error_and_never_a_panic() {
        assert_eq!(MidiFile::from_bytes(b""), Err(MidiFileError::NotMidiFile));
        assert_eq!(
            MidiFile::from_bytes(b"RIFF\0\0\0\0WAVE"),
            Err(MidiFileError::NotMidiFile)
        );
        // Every possible truncation of a valid file.
        for len in 4..HAND_WRITTEN.len() {
            let result = MidiFile::from_bytes(&HAND_WRITTEN[..len]);
            // A cut exactly after the header leaves a valid file with no
            // tracks; anywhere else, something is missing.
            if len != 14 {
                assert_eq!(result, Err(MidiFileError::Truncated), "cut at {len}");
            }
        }
        // Every single-byte corruption must be handled without panicking.
        for index in 0..HAND_WRITTEN.len() {
            for value in [0x00, 0x7F, 0x80, 0xF1, 0xFF] {
                let mut corrupted = HAND_WRITTEN.to_vec();
                corrupted[index] = value;
                let _ = MidiFile::from_bytes(&corrupted);
            }
        }
        let mut data_first = HAND_WRITTEN[..22].to_vec();
        data_first[21] = 3;
        data_first.extend_from_slice(&[0x00, 60, 100]);
        assert_eq!(
            MidiFile::from_bytes(&data_first),
            Err(MidiFileError::Malformed("data byte with no status"))
        );
    }
}
