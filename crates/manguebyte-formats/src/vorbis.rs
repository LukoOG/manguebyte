//! Ogg Vorbis.
//!
//! A decoder and an encoder written from the Vorbis I specification of the
//! Xiph.Org Foundation and from RFC 3533, which defines the Ogg container.
//!
//! The decoder decodes every stream the specification defines but one kind: floors
//! of type 1, residues of types 0, 1 and 2, codebooks with each lookup type,
//! channel coupling, both block sizes and the windows that join them, and
//! any number of channels from one to 255. A floor of type 0 is refused
//! with [`VorbisError::Unsupported`] when a packet uses it; no encoder in
//! use writes one, and libvorbis has not since before its version 1.0.
//!
//! ```no_run
//! let file = manguebyte_formats::vorbis::read_file("theme.ogg")?;
//! println!("{} Hz, {} channels, {} frames, from {}",
//!     file.sample_rate, file.buffer.num_channels(), file.buffer.num_frames(),
//!     file.vendor);
//! for (name, value) in &file.comments {
//!     println!("{name} = {value}");
//! }
//! # Ok::<(), manguebyte_formats::vorbis::VorbisError>(())
//! ```
//!
//! The functions here hold a whole file in memory. To decode block by block
//! and to seek, use [`AudioReader`](crate::AudioReader).
//!
//! # What comes out
//!
//! - The length is the one the stream states: the granule position of its
//!   last page, which trims the end of the last block, less any samples the
//!   granule position of the first page says to drop from the start. A file
//!   written by libvorbis decodes to exactly as many frames as were encoded.
//! - Channels are in the order WAV and FLAC use, not the order Vorbis
//!   stores them in: for 5.1, left, right, centre, LFE, rear left, rear
//!   right. Section 4.3.9 of the specification sets the Vorbis order for one
//!   to eight channels; beyond eight, channels come as they are stored.
//! - Samples are not clipped. A lossy codec does not give back the samples
//!   it was given, and loud audio can come back slightly beyond full scale.
//!
//! # Damage
//!
//! Every Ogg page carries a checksum. A page whose checksum does not match,
//! a page missing from the stream, or anything that is not a page where one
//! should be, is reported as an error. The audio of a page that passes
//! cannot be checked further, and is decoded as it is.
//!
//! # Limits
//!
//! - Only the first Vorbis stream of a file is read. Pages of other
//!   streams multiplexed with it are stepped over, and a stream chained
//!   after it is not read.
//! - The codebooks of a stream may declare up to 262,144 entries and hold
//!   up to 1,048,576 vector values between them. libvorbis uses a few
//!   thousand; the limit keeps a few bytes of header from asking for
//!   gigabytes.
//! - Memory follows the stream's channels and block size, which a header
//!   sets, and its codebooks. A stereo stream as libvorbis writes it at
//!   44.1 kHz takes about 300 KB to read; a stream of 255 channels and
//!   blocks of 8192 samples, the most the format allows, about 13 MB; a
//!   codebook of the most entries allowed, about 8 MB while it is read.
//! - An audio packet may be up to 64 KiB long, plus a byte per channel for
//!   each sample of a long block: several times what any encoder writes.
//!
//! # Writing
//!
//! [`write`] and [`AudioWriter::vorbis`](crate::AudioWriter::vorbis) encode
//! at a quality from 0 to 10 ([`VorbisOptions`]), with the comments and
//! pictures of a [`Metadata`]. The encoder is variable-bitrate:
//!
//! - Blocks of 2048 samples, 256 where a transient would smear into
//!   pre-echo (1024 and 128 below 24 kHz, 512 and 64 below 12 kHz), through
//!   the forward MDCT with the window the decoder puts on them.
//! - A masking threshold from a published model (see the `psy` module's
//!   sources: Zwicker's critical bands, Johnston's tonality, Terhardt's
//!   threshold in quiet) sets a floor of type 1 per channel; the residue
//!   is the spectrum over the floor, rounded, so that the noise lies at the
//!   floor, under the threshold by as much as the quality says.
//! - Stereo is coupled by the square polar mapping, after rounding, so
//!   that the decoder gets back exactly the values coded. The residue is
//!   of type 2 for one or two channels and of type 1 for more.
//! - The codebooks are Huffman codes designed from counts of what the
//!   encoder codes, sent in the setup header, which the encoder reads back
//!   with the decoder's parser before it writes any audio.
//! - The last page's granule position gives the stream exactly the length
//!   written, which this decoder, FFmpeg's and libvorbis give back.
//!
//! On the stereo test signal of this crate at 44.1 kHz, quality 0 comes out
//! at 77 kbit/s, 5 at 124 kbit/s and 10 at 173 kbit/s, with the noise 0.4,
//! 11 and 23 dB under the masking threshold of a model apart from the
//! encoder's, on average over the bands. It encodes 40 to 300 times faster
//! than real time on one core. Its memory follows the channels and does not
//! grow with the length of the stream; it allocates and does I/O, and so
//! belongs off the audio thread.
//!
//! The encoder is a simple one next to libvorbis, and spends more bits for
//! the same quality: it has no noise normalisation, no point stereo, no
//! masking from block to block and no bitrate management, and its
//! codebooks, some 2 KB of setup header, are the same for every stream.

use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Cursor, Read, Seek, Write};
use std::path::Path;

use manguebyte_core::AudioBuffer;

use crate::metadata::Metadata;

mod bits;
mod codebook;
mod decoder;
mod encode;
mod mdct;
mod reader;
mod setup;

pub use encode::VorbisOptions;
pub(crate) use encode::VorbisStream;

pub(crate) use reader::{VorbisReader, channel_order};
use setup::Limits;
pub(crate) use setup::{Comments, parse_comments};

/// The contents of an Ogg Vorbis file.
#[derive(Debug, Clone, PartialEq)]
pub struct VorbisFile {
    /// The decoded audio.
    pub buffer: AudioBuffer<f32>,
    /// Sample rate in hertz.
    pub sample_rate: u32,
    /// The name of the encoder, from the comment header.
    pub vendor: String,
    /// The comments, such as `TITLE` or `ARTIST`, in the order of the file.
    /// A name is ASCII and its case means nothing; a value is text of any
    /// kind. Bytes that are not UTF-8 are replaced.
    pub comments: Vec<(String, String)>,
    /// The same comments read into the fields of [`Metadata`], with the
    /// pictures `METADATA_BLOCK_PICTURE` comments carry.
    pub metadata: Metadata,
}

/// What went wrong while reading an Ogg Vorbis file.
#[derive(Debug)]
pub enum VorbisError {
    /// The underlying reader failed.
    Io(io::Error),
    /// The data is not an Ogg stream that starts with Vorbis.
    NotVorbis,
    /// The data ends before the headers do.
    Truncated,
    /// The stream breaks the rules of Ogg or of Vorbis.
    Malformed(&'static str),
    /// The stream is valid but uses something this decoder does not.
    Unsupported(&'static str),
    /// A page's checksum does not match its contents: the file is damaged.
    Corrupt,
}

impl fmt::Display for VorbisError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::NotVorbis => f.write_str("not an Ogg Vorbis file"),
            Self::Truncated => f.write_str("Ogg Vorbis file ends unexpectedly"),
            Self::Malformed(what) => write!(f, "malformed Ogg Vorbis file: {what}"),
            Self::Unsupported(what) => write!(f, "unsupported Ogg Vorbis file: {what}"),
            Self::Corrupt => {
                f.write_str("Ogg Vorbis file is damaged: a page checksum does not match")
            }
        }
    }
}

impl std::error::Error for VorbisError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for VorbisError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Reads an Ogg Vorbis file from `reader`. The whole stream is loaded into
/// memory.
pub fn read<R: Read>(mut reader: R) -> Result<VorbisFile, VorbisError> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    decode(&bytes)
}

/// Reads the Ogg Vorbis file at `path`.
pub fn read_file<P: AsRef<Path>>(path: P) -> Result<VorbisFile, VorbisError> {
    read(BufReader::new(File::open(path)?))
}

/// Writes `buffer` to `writer` as Ogg Vorbis of `sample_rate` hertz.
///
/// Samples are taken with full scale at `±1.0`; NaN becomes zero. The
/// encoder and what it makes are described in the
/// [module documentation](self#writing). To write block by block, use
/// [`AudioWriter::vorbis`](crate::AudioWriter::vorbis).
///
/// # Errors
///
/// [`VorbisError::Unsupported`] for options that
/// [`VorbisOptions::validate`] refuses, before anything is written, and
/// [`VorbisError::Io`] if `writer` fails.
pub fn write<W: Write + Seek>(
    mut writer: W,
    buffer: &AudioBuffer<f32>,
    sample_rate: u32,
    options: &VorbisOptions,
) -> Result<(), VorbisError> {
    encode::write_buffer(&mut writer, buffer, sample_rate, options)
}

/// Writes `buffer` to a new Ogg Vorbis file at `path`, replacing any file
/// there. The options are checked before the file is created.
pub fn write_file<P: AsRef<Path>>(
    path: P,
    buffer: &AudioBuffer<f32>,
    sample_rate: u32,
    options: &VorbisOptions,
) -> Result<(), VorbisError> {
    options.validate(buffer.num_channels(), sample_rate)?;
    let mut file = BufWriter::new(File::create(path)?);
    encode::write_buffer(&mut file, buffer, sample_rate, options)?;
    file.flush()?;
    Ok(())
}

fn decode(bytes: &[u8]) -> Result<VorbisFile, VorbisError> {
    let mut reader = VorbisReader::with_comments(Cursor::new(bytes))?;
    let channels = reader.num_channels();
    // The length is known up front in almost every file, but trusted only
    // as far as the data could hold it.
    let plausible = (bytes.len() as u64).saturating_mul(1_024);
    let expected = reader.frames().unwrap_or(0).min(plausible) as usize;
    let mut samples: Vec<Vec<f32>> = vec![Vec::with_capacity(expected); channels];
    let mut block = AudioBuffer::<f32>::new(channels, 4_096);
    loop {
        let got = reader.read(&mut block)?;
        if got == 0 {
            break;
        }
        for (out, channel) in samples.iter_mut().zip(block.iter_channels()) {
            out.extend_from_slice(&channel[..got]);
        }
    }
    let frames = samples.first().map_or(0, Vec::len);
    let mut buffer = AudioBuffer::new(channels, frames);
    for (out, channel) in buffer.iter_channels_mut().zip(&samples) {
        out.copy_from_slice(channel);
    }
    let comments = reader.take_comments();
    let metadata = Metadata::from_vorbis_comments(
        comments
            .comments
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
    );
    Ok(VorbisFile {
        buffer,
        sample_rate: reader.sample_rate(),
        vendor: comments.vendor,
        comments: comments.comments,
        metadata,
    })
}
