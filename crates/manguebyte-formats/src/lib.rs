//! Audio file formats.
//!
//! | Format | Read | Write |
//! |---|---|---|
//! | [`wav`] | 8 to 32-bit PCM, 32 and 64-bit float, RF64 | 16 and 24-bit PCM, 32-bit float; RF64 past 4 GiB when streaming |
//! | [`aiff`] | 1 to 32-bit PCM, either byte order, 32 and 64-bit float | 16 and 24-bit PCM, 32-bit float |
//! | [`flac`] | everything the format defines | 4 to 32 bits, 1 to 8 channels, levels 0 to 8 |
//! | [`vorbis`] | Ogg Vorbis: every stream but those with floors of type 0, 1 to 255 channels | no |
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
//! | Write | [`wav::write`], [`aiff::write`], [`flac::write`] | [`AudioWriter`] | [`RecordingWriter`] |
//!
//! [`AudioReader`] and [`AudioWriter`] keep one block in memory whatever the
//! length of the file, and the reader seeks to any frame. Both do I/O, which
//! an audio callback must never do, so [`StreamingPlayer`] and
//! [`RecordingWriter`] put them on a worker thread and leave the audio
//! thread with calls that neither block nor allocate.
//!
//! Every reader treats its input as untrusted: damaged or hostile data
//! produces an error, never a panic.

#![forbid(unsafe_code)]

pub mod aiff;
pub mod flac;
mod ogg;
mod pcm;
mod player;
mod reader;
mod recorder;
pub mod vorbis;
pub mod wav;
mod window;
mod writer;

pub use player::{PlayerConfig, PlayerThread, PlayerWorker, StreamingPlayer};
pub use reader::AudioReader;
pub use recorder::{RecorderConfig, RecorderThread, RecorderWorker, RecordingWriter};
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
}

impl FileFormat {
    /// Recognises a format from the first bytes of a file.
    pub fn detect(bytes: &[u8]) -> Option<Self> {
        match bytes.get(..4)? {
            b"RIFF" | b"RF64" => Some(Self::Wav),
            b"FORM" => Some(Self::Aiff),
            b"fLaC" => Some(Self::Flac),
            b"OggS" => Some(Self::OggVorbis),
            _ => None,
        }
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
    let (buffer, sample_rate, bits_per_sample) = match format {
        FileFormat::Wav => {
            let file = wav::read(bytes).map_err(FormatError::Wav)?;
            (file.buffer, file.sample_rate, file.bits_per_sample)
        }
        FileFormat::Aiff => {
            let file = aiff::read(bytes).map_err(FormatError::Aiff)?;
            (file.buffer, file.sample_rate, file.bits_per_sample)
        }
        FileFormat::Flac => {
            let file = flac::read(bytes).map_err(FormatError::Flac)?;
            (file.buffer, file.sample_rate, file.bits_per_sample)
        }
        FileFormat::OggVorbis => {
            let file = vorbis::read(bytes).map_err(FormatError::Vorbis)?;
            (file.buffer, file.sample_rate, LOSSY_BITS)
        }
    };
    Ok(AudioFile {
        buffer,
        sample_rate,
        bits_per_sample,
        format,
    })
}

/// Reads the audio file at `path`, whatever its format. The format is
/// recognised from the file's contents, not from its name.
pub fn read_file<P: AsRef<Path>>(path: P) -> Result<AudioFile, FormatError> {
    decode(&std::fs::read(path)?)
}
