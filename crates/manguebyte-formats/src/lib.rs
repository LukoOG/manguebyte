//! Audio file formats.
//!
//! | Format | Read | Write |
//! |---|---|---|
//! | [`wav`] | 8 to 32-bit PCM, 32 and 64-bit float, RF64 | 16 and 24-bit PCM, 32-bit float; RF64 past 4 GiB when streaming |
//! | [`aiff`] | 1 to 32-bit PCM, either byte order, 32 and 64-bit float | 16 and 24-bit PCM, 32-bit float |
//! | [`flac`] | everything the format defines | 4 to 32 bits, 1 to 8 channels, levels 0 to 8 |
//! | [`vorbis`] | Ogg Vorbis: every stream but those with floors of type 0, 1 to 255 channels | variable bitrate, qualities 0 to 10, 1 to 255 channels |
//! | [`opus`] | Ogg Opus: SILK, CELT and hybrid, every frame size and bandwidth, up to 255 channels | CELT only, constant and variable bitrate, frames of 2.5 to 20 ms, 1 to 255 channels, any input rate, gapless |
//! | [`mp3`] | MPEG-1, 2 and 2.5 layer III, every rate and stereo coding, gapless with a LAME tag; layers I and II | MPEG-1 layer III at 32 to 48 kHz, constant and variable bitrate, gapless |
//!
//! [`read_file`] and [`decode`] work out which format they have been given,
//! so a program that only wants the audio does not need to care:
//!
//! ```no_run
//! let file = manguebyte_formats::read_file("take-3.flac")?;
//! println!("{:?}: {} Hz, {} channels, {} frames", file.format,
//!     file.sample_rate, file.buffer.num_channels(), file.buffer.num_frames());
//! # Ok::<(), manguebyte_formats::FormatError>(())
//! ```
//!
//! Those load a whole file into memory. There are three ways to work with
//! audio, depending on where the code runs and how long the audio is:
//!
//! | | Whole file | Block by block | From the audio thread |
//! |---|---|---|---|
//! | Read | [`read_file`], [`decode`] | [`AudioReader`] | [`StreamingPlayer`] |
//! | Write | [`wav::write`], [`aiff::write`], [`flac::write`], [`vorbis::write`], [`mp3::write`] | [`AudioWriter`] | [`RecordingWriter`] |
//!
//! [`AudioReader`] and [`AudioWriter`] keep one block in memory whatever the
//! length of the file, and the reader seeks to any frame. Both do I/O, which
//! an audio callback must never do, so [`StreamingPlayer`] and
//! [`RecordingWriter`] put them on a worker thread and leave the audio
//! thread with calls that neither block nor allocate.
//!
//! What a file says beside its audio, its tags, cue points, loops,
//! Broadcast Wave fields and pictures, comes in one model for every format,
//! [`Metadata`]: in [`AudioFile::metadata`] and [`AudioReader::metadata`]
//! when reading, and to [`wav::write_with_metadata`],
//! [`aiff::write_with_metadata`], [`FlacOptions::with_metadata`](flac::FlacOptions::with_metadata),
//! [`VorbisOptions::with_metadata`](vorbis::VorbisOptions::with_metadata),
//! [`Mp3Options::with_metadata`](mp3::Mp3Options::with_metadata) and
//! [`AudioWriter::set_metadata`] when writing. The [`metadata`] module
//! says where each format keeps each field.
//!
//! [`Thumbnail`](thumbnail::Thumbnail) keeps the waveform overview an
//! editor draws a file with: the lowest, highest and RMS level of every
//! stretch, at every zoom, built while the file is read and cached in a
//! compact file of its own.
//!
//! [`TransportSource`] puts a transport around the player: play, pause,
//! stop, seek and loop, worked from any thread through a
//! [`TransportControl`], and an
//! [`AudioSource`](manguebyte_core::source::AudioSource) that a device
//! callback, a graph or a mixer pulls from.
//!
//! Every reader treats its input as untrusted: damaged or hostile data
//! produces an error, never a panic.

// Only the system's codecs need unsafe code, to call them; without the
// feature that does, none is allowed, and with it, only `platform` may.
#![cfg_attr(not(feature = "platform-codecs"), forbid(unsafe_code))]
#![cfg_attr(feature = "platform-codecs", deny(unsafe_code))]

pub mod aiff;
pub mod flac;
pub mod metadata;
pub mod mp3;
mod ogg;
pub mod opus;
mod pcm;
pub mod platform;
mod player;
mod psy;
mod reader;
mod recorder;
pub mod thumbnail;
mod transport;
pub mod vorbis;
pub mod wav;
mod window;
mod writer;

pub use metadata::Metadata;
pub use player::{PlayerConfig, PlayerThread, PlayerWorker, StreamingPlayer};
pub use reader::AudioReader;
pub use recorder::{RecorderConfig, RecorderThread, RecorderWorker, RecordingWriter};
pub use transport::{TransportControl, TransportSource};
pub use writer::AudioWriter;

use std::fmt;
use std::io;
use std::path::Path;

use manguebyte_core::AudioBuffer;

/// A file format this crate can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileFormat {
    /// RIFF/WAVE.
    Wav,
    /// AIFF or AIFF-C.
    Aiff,
    /// FLAC.
    Flac,
    /// Vorbis in an Ogg container.
    OggVorbis,
    /// Opus in an Ogg container.
    OggOpus,
    /// MPEG audio: layer III, which is MP3, and layers I and II.
    Mp3,
    /// An MPEG-4 file (`.m4a`, `.mp4`), which holds AAC or ALAC. Decoded
    /// by the system's codecs: see [`platform`].
    Mp4,
    /// AAC in ADTS frames (`.aac`). Decoded by the system's codecs.
    Aac,
    /// Windows Media Audio, in an ASF file (`.wma`). Decoded by the
    /// system's codecs, on Windows.
    Wma,
    /// Apple's Core Audio Format (`.caf`). Decoded by the system's codecs,
    /// on macOS.
    Caf,
}

/// The GUID an ASF file starts with, as its bytes are stored.
const ASF_HEADER: [u8; 16] = [
    0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE, 0x6C,
];

impl FileFormat {
    /// How many bytes [`detect`](Self::detect) needs to recognise every
    /// format.
    pub const DETECT_LEN: usize = 64;

    /// Recognises a format from the first bytes of a file:
    /// [`DETECT_LEN`](Self::DETECT_LEN) are enough for every format.
    ///
    /// MP3 has no marker of its own. A file is taken for one if it starts
    /// with an ID3v2 tag, which is how most MP3 files start, or with the
    /// header of an MPEG audio frame. An MPEG-4 file is told by the `ftyp`
    /// box it starts with, ADTS by the sync word of its frames, which MPEG
    /// audio frames do not share.
    ///
    /// An Ogg file is taken for Opus if its first page holds the
    /// identification header of Opus, and for Vorbis otherwise.
    pub fn detect(bytes: &[u8]) -> Option<Self> {
        match bytes.get(..4)? {
            b"RIFF" | b"RF64" => Some(Self::Wav),
            b"FORM" => Some(Self::Aiff),
            b"fLaC" => Some(Self::Flac),
            b"OggS" if opus::starts_opus(bytes) => Some(Self::OggOpus),
            b"OggS" => Some(Self::OggVorbis),
            b"caff" => Some(Self::Caf),
            _ if bytes.get(4..8) == Some(b"ftyp") => Some(Self::Mp4),
            _ if bytes.get(..16) == Some(&ASF_HEADER[..]) => Some(Self::Wma),
            // Twelve bits of sync, then the layer field, which is zero.
            &[0xFF, b, _, _] if b & 0xF6 == 0xF0 => Some(Self::Aac),
            &[a, b, c, d] => {
                let mp3 = mp3::id3v2_len(bytes).is_some()
                    || mp3::FrameHeader::parse([a, b, c, d]).is_some();
                mp3.then_some(Self::Mp3)
            }
            _ => None,
        }
    }

    /// Whether the system's codecs decode this format, not this crate:
    /// see [`platform`].
    pub fn is_platform(self) -> bool {
        matches!(self, Self::Mp4 | Self::Aac | Self::Wma | Self::Caf)
    }
}

/// What [`AudioFile::bits_per_sample`] and [`AudioReader::bits_per_sample`]
/// say for a lossy format. It stores no samples, only what they are made
/// back from, and its decoder works out each one as a 32-bit float.
const LOSSY_BITS: u16 = 32;

/// An audio file of any supported format, decoded.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioFile {
    /// The audio, with integer full scale mapped to `±1.0`.
    pub buffer: AudioBuffer<f32>,
    /// Sample rate in hertz.
    pub sample_rate: u32,
    /// Bit depth the samples were stored with. A lossy format stores no
    /// samples; for it this is 32, the depth of the floating-point samples
    /// its decoder gives.
    pub bits_per_sample: u16,
    /// The format the file was in.
    pub format: FileFormat,
    /// Tags, markers, loops, pictures and the chunks this crate keeps
    /// without interpreting them.
    pub metadata: Metadata,
}

/// What went wrong while reading an audio file of unknown format, or while
/// writing one with [`AudioWriter`].
#[derive(Debug)]
pub enum FormatError {
    /// The file could not be read or written.
    ///
    /// When writing FLAC, an error of kind
    /// [`InvalidInput`](io::ErrorKind::InvalidInput) may instead mean options
    /// outside the [limits](flac::FlacOptions#limits); a destination can
    /// fail with that kind too. [`FlacOptions::validate`](flac::FlacOptions::validate)
    /// checks the options on their own.
    Io(io::Error),
    /// The data is not in a format this crate knows.
    Unrecognized,
    /// The file is a WAV file with a problem.
    Wav(wav::WavError),
    /// The file is an AIFF file with a problem.
    Aiff(aiff::AiffError),
    /// The file is a FLAC file with a problem.
    Flac(flac::FlacError),
    /// The file is an Ogg file with a problem, or one that holds no Vorbis.
    Vorbis(vorbis::VorbisError),
    /// The file is an MP3 file with a problem.
    Mp3(mp3::Mp3Error),
    /// The file is an Ogg Opus file with a problem.
    Opus(opus::OpusError),
    /// What was asked cannot be done with this format, or on this system:
    /// the message says what. A format the system's codecs decode is
    /// refused with this where they cannot: see [`platform`].
    Unsupported(&'static str),
    /// The system's codecs failed on a format this crate hands to them.
    Platform(platform::PlatformError),
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Unrecognized => f.write_str("not a recognised audio file format"),
            Self::Wav(error) => write!(f, "{error}"),
            Self::Aiff(error) => write!(f, "{error}"),
            Self::Flac(error) => write!(f, "{error}"),
            Self::Vorbis(error) => write!(f, "{error}"),
            Self::Mp3(error) => write!(f, "{error}"),
            Self::Opus(error) => write!(f, "{error}"),
            Self::Unsupported(what) => write!(f, "unsupported: {what}"),
            Self::Platform(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for FormatError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Unrecognized => None,
            Self::Wav(error) => Some(error),
            Self::Aiff(error) => Some(error),
            Self::Flac(error) => Some(error),
            Self::Vorbis(error) => Some(error),
            Self::Mp3(error) => Some(error),
            Self::Opus(error) => Some(error),
            Self::Unsupported(_) => None,
            Self::Platform(error) => Some(error),
        }
    }
}

impl From<io::Error> for FormatError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Decodes an audio file held in memory, whatever its format.
pub fn decode(bytes: &[u8]) -> Result<AudioFile, FormatError> {
    let format = FileFormat::detect(bytes).ok_or(FormatError::Unrecognized)?;
    let (buffer, sample_rate, bits_per_sample, metadata) = match format {
        FileFormat::Wav => {
            let file = wav::read(bytes).map_err(FormatError::Wav)?;
            (
                file.buffer,
                file.sample_rate,
                file.bits_per_sample,
                file.metadata,
            )
        }
        FileFormat::Aiff => {
            let file = aiff::read(bytes).map_err(FormatError::Aiff)?;
            (
                file.buffer,
                file.sample_rate,
                file.bits_per_sample,
                file.metadata,
            )
        }
        FileFormat::Flac => {
            let file = flac::read(bytes).map_err(FormatError::Flac)?;
            (
                file.buffer,
                file.sample_rate,
                file.bits_per_sample,
                file.metadata,
            )
        }
        FileFormat::OggVorbis => {
            let file = vorbis::read(bytes).map_err(FormatError::Vorbis)?;
            (file.buffer, file.sample_rate, LOSSY_BITS, file.metadata)
        }
        FileFormat::Mp3 => {
            let file = mp3::read(bytes).map_err(FormatError::Mp3)?;
            (file.buffer, file.sample_rate, LOSSY_BITS, file.metadata)
        }
        FileFormat::OggOpus => {
            let file = opus::read(bytes).map_err(FormatError::Opus)?;
            (file.buffer, opus::SAMPLE_RATE, LOSSY_BITS, file.metadata)
        }
        FileFormat::Mp4 | FileFormat::Aac | FileFormat::Wma | FileFormat::Caf => {
            let mut reader = platform::PlatformReader::open_bytes(bytes.to_vec(), format)?;
            let buffer = platform::read_all(&mut reader)?;
            (
                buffer,
                reader.sample_rate(),
                LOSSY_BITS,
                Metadata::default(),
            )
        }
    };
    Ok(AudioFile {
        buffer,
        sample_rate,
        bits_per_sample,
        format,
        metadata,
    })
}

/// Reads the audio file at `path`, whatever its format. The format is
/// recognised from the file's contents, not from its name.
///
/// A format the system's codecs decode is read from the file by them, with
/// the `platform-codecs` feature: see [`platform`].
pub fn read_file<P: AsRef<Path>>(path: P) -> Result<AudioFile, FormatError> {
    use std::io::Read;

    let path = path.as_ref();
    let mut file = std::fs::File::open(path)?;
    let mut head = [0u8; FileFormat::DETECT_LEN];
    let have = pcm::read_up_to(&mut file, &mut head)?;
    match FileFormat::detect(&head[..have]) {
        Some(format) if format.is_platform() => {
            drop(file);
            let mut reader = platform::PlatformReader::open_path(path, format)?;
            let buffer = platform::read_all(&mut reader)?;
            Ok(AudioFile {
                buffer,
                sample_rate: reader.sample_rate(),
                bits_per_sample: LOSSY_BITS,
                format,
                metadata: Metadata::default(),
            })
        }
        _ => {
            let mut bytes = head[..have].to_vec();
            file.read_to_end(&mut bytes)?;
            decode(&bytes)
        }
    }
}
