//! What files carry beside their audio: tags, markers, loops and pictures.
//!
//! Every format keeps these its own way. [`Metadata`] is one model for all
//! of them: the fields most programs look for, typed, and everything else
//! kept as it was found so that reading a file and writing it again loses
//! nothing this crate does not understand.
//!
//! ```
//! use manguebyte_core::AudioBuffer;
//! use manguebyte_formats::metadata::{CuePoint, Loop, Metadata};
//! use manguebyte_formats::wav::{self, WavFormat};
//!
//! let mut metadata = Metadata::default();
//! metadata.title = Some("Take 3".into());
//! metadata.loops.push(Loop::forward(100, 900));
//! metadata.cue_points.push(CuePoint::new(1, 400).with_label("Chorus"));
//!
//! let audio = AudioBuffer::<f32>::new(1, 1_000);
//! let mut bytes = Vec::new();
//! wav::write_with_metadata(&mut bytes, &audio, 48_000, WavFormat::Int24, &metadata).unwrap();
//!
//! let file = wav::read(bytes.as_slice()).unwrap();
//! assert_eq!(file.metadata.title.as_deref(), Some("Take 3"));
//! assert_eq!(file.metadata.loops, metadata.loops);
//! assert_eq!(file.metadata.cue_points[0].label.as_deref(), Some("Chorus"));
//! ```
//!
//! # Where each field lives
//!
//! | Field | WAV | AIFF | FLAC and Ogg Vorbis | MP3 |
//! |---|---|---|---|---|
//! | `title` | `LIST/INFO` `INAM` | `NAME` | `TITLE` | ID3 `TIT2`, ID3v1 |
//! | `artist` | `IART` | `AUTH` | `ARTIST` | `TPE1`, ID3v1 |
//! | `album` | `IPRD` | | `ALBUM` | `TALB`, ID3v1 |
//! | `date` | `ICRD` | | `DATE` | `TDRC`, `TYER`, ID3v1 |
//! | `track`, `track_total` | `ITRK` | | `TRACKNUMBER`, `TRACKTOTAL` | `TRCK`, ID3v1.1 |
//! | `genre` | `IGNR` | | `GENRE` | `TCON`, ID3v1 |
//! | `comment` | `ICMT` | `ANNO` | `COMMENT` | `COMM`, ID3v1 |
//! | `copyright` | `ICOP` | `(c) ` | `COPYRIGHT` | `TCOP` |
//! | `bpm` | tempo of an `acid` chunk | | `BPM` | `TBPM` |
//! | `key` | | | `KEY` | `TKEY` |
//! | `isrc` | | | `ISRC` | `TSRC` |
//! | `originator`, `time_reference`, `broadcast` | `bext` | | | |
//! | `ixml` | `iXML` | | | |
//! | `loops` | `smpl` | `INST` and `MARK` | | |
//! | `cue_points` | `cue ` with `LIST/adtl` labels | `MARK` | | |
//! | `instrument` | `inst`, and the root of `smpl` | `INST` | | |
//! | `pictures` | | | `PICTURE` blocks, `METADATA_BLOCK_PICTURE` | `APIC` |
//! | `tags` | other `INFO` items | | other comments | other text frames |
//! | `chunks` | chunks not listed here | chunks not listed here | blocks not listed here | |
//!
//! A blank is a field the format has no place for: it is not read from such
//! a file and not written to one. MP3 is written with an ID3v2.4 tag in
//! UTF-8, without ID3v1, `TYER` or the dimensions of a picture, which `APIC`
//! has no place for. RIFF's `INFO` defines no ISRC: its `ISRC`
//! item is the *source* of a file, and comes back among the `tags` under
//! that name.
//!
//! # Round trips
//!
//! A file read and written again in its own format keeps every field above
//! and every chunk this crate does not interpret, byte for byte, in
//! [`chunks`](Metadata::chunks). The chunks it does interpret are written
//! again from the fields, in the layout the specifications give, so a field
//! the model has no room for is lost: the manufacturer, product and SMPTE
//! offset of a `smpl` chunk, the fractions of its loops, and loops past the
//! second in AIFF. The chunks the audio itself needs (`fmt `, `data`,
//! `COMM`, `SSND`, stream information, seek tables) are never in
//! `chunks`: the writers make them.
//!
//! A file with loops always comes back with an
//! [`instrument`](Metadata::instrument): the `smpl` and `INST` chunks that
//! hold loops also state a root note, middle C when none was given. WAV's
//! track number is written as `ITRK`, and read from `ITRK` or, as FFmpeg
//! writes it, `IPRT`.
//!
//! A chunk of another format is not written: the `chunks` of a WAV file are
//! left out of an AIFF or FLAC file made from it.
//!
//! # Hostile input
//!
//! A damaged or hostile file never makes reading fail because of its
//! metadata: what cannot be understood is left out, and the audio is read
//! as before. Nothing is allocated beyond what the file holds, a chunk of
//! more than 16 MiB is not kept, and no more than 64 MiB of metadata is
//! kept from one file.

use std::collections::BTreeMap;

mod id3;
mod iff;
mod riff;
mod vorbis_comment;

pub(crate) use id3::{read_id3v1, read_id3v2, write_id3v2};
pub(crate) use iff::{IffMetadata, build_iff};
pub(crate) use riff::{RiffMetadata, build_riff};
pub(crate) use vorbis_comment::{
    flac_picture_block, parse_flac_picture, parse_vorbis_comment_block, picture_comment,
    vorbis_tags,
};

/// The largest chunk or block whose contents are kept.
pub(crate) const MAX_CHUNK: u64 = 16 << 20;

/// The most metadata kept from one file.
pub(crate) const MAX_TOTAL: u64 = 64 << 20;

/// What a file says about its audio, in any format.
///
/// The module documentation says where each field is kept in each format.
/// Text is whatever the file holds; a file whose text is not UTF-8 is read
/// as Latin-1, which is what older software wrote.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Metadata {
    /// The name of the piece.
    pub title: Option<String>,
    /// Who performed or made it.
    pub artist: Option<String>,
    /// The album or product it belongs to.
    pub album: Option<String>,
    /// When it was made, as the file writes it: often a year, or
    /// `YYYY-MM-DD`.
    pub date: Option<String>,
    /// Its number on the album.
    pub track: Option<u32>,
    /// How many tracks the album has.
    pub track_total: Option<u32>,
    /// Its genre, as text.
    pub genre: Option<String>,
    /// A comment or description.
    pub comment: Option<String>,
    /// The copyright notice.
    pub copyright: Option<String>,
    /// Tempo in beats per minute.
    pub bpm: Option<f32>,
    /// Musical key, as text, such as `Am` or `F#`.
    pub key: Option<String>,
    /// International Standard Recording Code.
    pub isrc: Option<String>,
    /// Who made the recording: the originator of a Broadcast Wave file.
    pub originator: Option<String>,
    /// Where the recording starts, in samples since midnight: the time
    /// reference of a Broadcast Wave file.
    pub time_reference: Option<u64>,
    /// The rest of a Broadcast Wave `bext` chunk.
    pub broadcast: Option<Broadcast>,
    /// The `iXML` chunk of a WAV file, as text. It is not interpreted, only
    /// kept and written back.
    pub ixml: Option<String>,
    /// How a sampler should play the sound.
    pub instrument: Option<Instrument>,
    /// Loops, in frames.
    pub loops: Vec<Loop>,
    /// Markers, in frames.
    pub cue_points: Vec<CuePoint>,
    /// Pictures, such as a cover.
    pub pictures: Vec<Picture>,
    /// Text the fields above have no place for, as name and value, in the
    /// order of the file. Names are the format's: an `INFO` identifier such
    /// as `ISFT`, a Vorbis comment name such as `ALBUMARTIST`, an ID3 frame
    /// identifier such as `TPE2`, or the description of an ID3 `TXXX`
    /// frame. A name the fields use comes here when a file gives it twice.
    pub tags: Vec<(String, String)>,
    /// The chunks, or FLAC metadata blocks, that this crate does not
    /// interpret, as they were.
    pub chunks: Vec<RawChunk>,
}

impl Metadata {
    /// Whether there is nothing here at all.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The value of the first tag called `name`, compared without regard to
    /// case.
    pub fn tag(&self, name: &str) -> Option<&str> {
        self.tags
            .iter()
            .find(|(tag, _)| tag.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Reads Vorbis comments, as FLAC and Ogg Vorbis files hold them, into
    /// the fields; the comments that name none of them go to
    /// [`tags`](Self::tags). Pictures carried in `METADATA_BLOCK_PICTURE`
    /// comments, as Ogg files carry them, are decoded.
    pub fn from_vorbis_comments<'a, I>(comments: I) -> Self
    where
        I: IntoIterator<Item = (&'a str, &'a str)>,
    {
        let mut metadata = Self::default();
        let mut budget = Budget::new();
        for (name, value) in comments {
            vorbis_comment::apply(&mut metadata, name, value, &mut budget);
        }
        metadata
    }

    /// The fields as Vorbis comments, followed by [`tags`](Self::tags):
    /// what [`FlacOptions::with_metadata`](crate::flac::FlacOptions::with_metadata)
    /// writes. Pictures, loops, cue points and the Broadcast Wave fields
    /// have no comment and are not among them.
    pub fn to_vorbis_comments(&self) -> Vec<(String, String)> {
        vorbis_tags(self)
    }

    /// Fills the fields of `self` that are not set from `other`, and adds
    /// the lists of `other` to those of `self` where `self` has none.
    pub(crate) fn fill_from(&mut self, other: Self) {
        fn fill<T>(slot: &mut Option<T>, value: Option<T>) {
            if slot.is_none() {
                *slot = value;
            }
        }
        fill(&mut self.title, other.title);
        fill(&mut self.artist, other.artist);
        fill(&mut self.album, other.album);
        fill(&mut self.date, other.date);
        fill(&mut self.track, other.track);
        fill(&mut self.track_total, other.track_total);
        fill(&mut self.genre, other.genre);
        fill(&mut self.comment, other.comment);
        fill(&mut self.copyright, other.copyright);
        fill(&mut self.bpm, other.bpm);
        fill(&mut self.key, other.key);
        fill(&mut self.isrc, other.isrc);
        fill(&mut self.originator, other.originator);
        fill(&mut self.time_reference, other.time_reference);
        fill(&mut self.broadcast, other.broadcast);
        fill(&mut self.ixml, other.ixml);
        fill(&mut self.instrument, other.instrument);
        if self.loops.is_empty() {
            self.loops = other.loops;
        }
        if self.cue_points.is_empty() {
            self.cue_points = other.cue_points;
        }
        if self.pictures.is_empty() {
            self.pictures = other.pictures;
        }
        self.tags.extend(other.tags);
        self.chunks.extend(other.chunks);
    }
}

/// The fields of a Broadcast Wave `bext` chunk (EBU Tech 3285) that
/// [`Metadata`] does not have fields of its own for.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Broadcast {
    /// A description of the sound, up to 256 bytes.
    pub description: String,
    /// A reference the originator gave the recording, up to 32 bytes.
    pub originator_reference: String,
    /// When it was made, `yyyy-mm-dd`.
    pub origination_date: String,
    /// When it was made, `hh:mm:ss`.
    pub origination_time: String,
    /// The version of the chunk: 0, 1 (which added the UMID) or 2 (which
    /// added the loudness).
    pub version: u16,
    /// A SMPTE UMID (SMPTE 330M), 64 bytes, zeros where there is none.
    pub umid: Vec<u8>,
    /// The loudness, from version 2 on.
    pub loudness: Option<Loudness>,
    /// The coding history: one line of text for each process the sound went
    /// through.
    pub coding_history: String,
}

/// The loudness a version 2 `bext` chunk states (EBU R 128), each to a
/// hundredth.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Loudness {
    /// Integrated loudness, in LUFS.
    pub integrated: f32,
    /// Loudness range, in LU.
    pub range: f32,
    /// Maximum true peak level, in dBTP.
    pub max_true_peak: f32,
    /// Highest momentary loudness, in LUFS.
    pub max_momentary: f32,
    /// Highest short-term loudness, in LUFS.
    pub max_short_term: f32,
}

/// How a sampler should play a sound: the `INST` chunk of AIFF, and the
/// `inst` and `smpl` chunks of WAV.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Instrument {
    /// The MIDI note the sound plays at when it is not transposed.
    pub root_note: u8,
    /// How far above the root note it is, in cents, from -50 to 50.
    pub fine_tune: f32,
    /// Gain to play it at, in decibels.
    pub gain_db: f32,
    /// The lowest and highest MIDI notes it is played for.
    pub notes: (u8, u8),
    /// The lowest and highest velocities it is played for.
    pub velocities: (u8, u8),
}

impl Default for Instrument {
    /// Middle C, in tune, over every note and velocity.
    fn default() -> Self {
        Self {
            root_note: 60,
            fine_tune: 0.0,
            gain_db: 0.0,
            notes: (0, 127),
            velocities: (1, 127),
        }
    }
}

/// How a loop is played.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LoopKind {
    /// From its start to its end, and again.
    Forward,
    /// Forward, then backward, and again.
    PingPong,
    /// From its end to its start, and again.
    Backward,
    /// A kind a manufacturer defined, by its number in a WAV `smpl` chunk.
    Other(u32),
}

/// A loop, in frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Loop {
    /// The first frame of the loop.
    pub start: u64,
    /// The frame after its last one. A WAV `smpl` chunk states the last
    /// frame itself, one less.
    pub end: u64,
    /// How it is played.
    pub kind: LoopKind,
    /// How many times it is played; zero is for ever.
    pub play_count: u32,
}

impl Loop {
    /// A loop from `start` up to `end`, not included, played forward for
    /// ever.
    pub fn forward(start: u64, end: u64) -> Self {
        Self {
            start,
            end,
            kind: LoopKind::Forward,
            play_count: 0,
        }
    }
}

/// A marker at a frame, with a name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CuePoint {
    /// The number that tells it from the others. AIFF numbers markers from
    /// 1 to 32767; a cue point outside that range, or with a number another
    /// one has, is numbered again when written to AIFF.
    pub id: u32,
    /// The frame it marks.
    pub position: u64,
    /// For a region rather than a point, how many frames it spans; zero for
    /// a point. Only WAV keeps it, in an `ltxt` entry.
    pub length: u64,
    /// Its name.
    pub label: Option<String>,
}

impl CuePoint {
    /// A cue point without a name.
    pub fn new(id: u32, position: u64) -> Self {
        Self {
            id,
            position,
            length: 0,
            label: None,
        }
    }

    /// The same cue point, with a name.
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

/// A picture, as FLAC's `PICTURE` block and ID3's `APIC` frame hold one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Picture {
    /// What it shows, by the numbers of ID3v2 and FLAC: 3 is the front
    /// cover, 4 the back, 0 anything else.
    pub kind: u32,
    /// Its MIME type, such as `image/png`.
    pub mime: String,
    /// What it is, in words.
    pub description: String,
    /// Width in pixels, zero if not known.
    pub width: u32,
    /// Height in pixels, zero if not known.
    pub height: u32,
    /// Bits per pixel, zero if not known.
    pub depth: u32,
    /// For an indexed image, the number of colours; zero otherwise.
    pub colors: u32,
    /// The image file itself.
    pub data: Vec<u8>,
}

/// What a raw chunk belongs to, and its identifier there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChunkId {
    /// A chunk of a RIFF (WAV) file, little-endian.
    Riff([u8; 4]),
    /// A chunk of an AIFF file, big-endian.
    Aiff([u8; 4]),
    /// A FLAC metadata block, by its type, such as 2 for an application
    /// block or 5 for a cue sheet.
    Flac(u8),
}

/// A chunk kept as it was found: its body, without the header.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RawChunk {
    /// What it is.
    pub id: ChunkId,
    /// Its contents.
    pub data: Vec<u8>,
}

/// How much metadata a reader may still keep, so that a file cannot make
/// it keep more than [`MAX_TOTAL`].
#[derive(Debug)]
pub(crate) struct Budget {
    left: u64,
}

impl Budget {
    pub(crate) fn new() -> Self {
        Self { left: MAX_TOTAL }
    }

    /// Whether `len` more bytes may be kept; if so, they are counted.
    pub(crate) fn take(&mut self, len: u64) -> bool {
        if len <= MAX_CHUNK && len <= self.left {
            self.left -= len;
            true
        } else {
            false
        }
    }
}

/// Text as a file holds it: up to the first NUL, as UTF-8 if it is valid
/// UTF-8 and as Latin-1 if it is not.
pub(crate) fn text(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(bytes.len());
    latin1_or_utf8(&bytes[..end])
}

/// UTF-8 if it is valid UTF-8, Latin-1 if it is not.
pub(crate) fn latin1_or_utf8(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => bytes.iter().map(|&byte| char::from(byte)).collect(),
    }
}

/// The number at the start of `text`, as track numbers are written:
/// `"3"`, `"03"` or `"3/12"`. Also the number after the slash.
pub(crate) fn track_numbers(text: &str) -> (Option<u32>, Option<u32>) {
    let mut parts = text.trim().splitn(2, '/');
    let number = |part: Option<&str>| part.and_then(|part| part.trim().parse::<u32>().ok());
    (number(parts.next()), number(parts.next()))
}

/// A tempo written as text, if it is a positive number.
pub(crate) fn bpm(text: &str) -> Option<f32> {
    text.trim()
        .parse::<f32>()
        .ok()
        .filter(|bpm| bpm.is_finite() && *bpm > 0.0)
}

/// Writes a tempo as text: a whole number without a fraction.
pub(crate) fn bpm_text(bpm: f32) -> String {
    if bpm.fract() == 0.0 {
        format!("{bpm:.0}")
    } else {
        bpm.to_string()
    }
}

/// Labels for cue points, gathered by number while a file is read, and
/// joined to the points at the end, since the two may come in either order.
#[derive(Debug, Default)]
pub(crate) struct CueLabels {
    pub(crate) labels: BTreeMap<u32, String>,
    pub(crate) lengths: BTreeMap<u32, u64>,
}

impl CueLabels {
    pub(crate) fn apply(self, points: &mut [CuePoint]) {
        for point in points {
            if let Some(label) = self.labels.get(&point.id) {
                point.label = Some(label.clone());
            }
            if let Some(&length) = self.lengths.get(&point.id) {
                point.length = length;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_utf8_or_latin1_and_stops_at_a_nul() {
        assert_eq!(text(b"Caf\xC3\xA9\0junk"), "Café");
        assert_eq!(text(b"Caf\xE9"), "Café");
        assert_eq!(text(b""), "");
    }

    #[test]
    fn track_numbers_and_tempos_are_read_as_written() {
        assert_eq!(track_numbers("03/12"), (Some(3), Some(12)));
        assert_eq!(track_numbers("7"), (Some(7), None));
        assert_eq!(track_numbers("side A"), (None, None));
        assert_eq!(bpm("120"), Some(120.0));
        assert_eq!(bpm(" 98.5 "), Some(98.5));
        assert_eq!(bpm("-3"), None);
        assert_eq!(bpm("NaN"), None);
        assert_eq!(bpm_text(120.0), "120");
        assert_eq!(bpm_text(98.5), "98.5");
    }

    #[test]
    fn the_budget_refuses_large_chunks_and_runs_out() {
        let mut budget = Budget::new();
        assert!(!budget.take(MAX_CHUNK + 1));
        for _ in 0..4 {
            assert!(budget.take(MAX_CHUNK));
        }
        assert!(!budget.take(1));
    }
}
