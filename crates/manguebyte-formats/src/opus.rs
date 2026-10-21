//! Ogg Opus.
//!
//! A decoder written from RFC 6716, which defines Opus, its updates in RFC
//! 8251, and RFC 7845, which puts Opus in Ogg (RFC 3533). It decodes all of
//! Opus: the SILK layer at 8, 12 and 16 kHz, mono and stereo with the
//! prediction of the side from the mid; the CELT layer at every bandwidth
//! from narrow to full band, frames of 2.5 to 20 ms, transients, intensity
//! and dual stereo, the post-filter; hybrid frames of both; the redundant
//! frames that smooth a change of mode; packets of up to 120 ms, of every
//! framing; and multistream packets of up to 255 channels with the channel
//! mappings of RFC 7845. An encoder of the CELT layer alone, written from
//! the same RFCs, writes Ogg Opus too: see [Writing](#writing).
//!
//! ```no_run
//! let file = manguebyte_formats::opus::read_file("voice.opus")?;
//! println!("{} channels, {} frames at 48 kHz, from {}",
//!     file.buffer.num_channels(), file.buffer.num_frames(), file.vendor);
//! # Ok::<(), manguebyte_formats::opus::OpusError>(())
//! ```
//!
//! The functions here hold a whole file in memory. To decode block by block
//! and to seek, use [`AudioReader`](crate::AudioReader).
//!
//! # What comes out
//!
//! - Audio at 48 kHz, the rate of Opus, whatever the rate the encoder was
//!   given: [`OpusFile::input_rate`] says what that was, for information.
//! - The length the stream states: the pre-skip of the header, the delay of
//!   the encoder, is dropped from the start, and the granule position of
//!   the last page trims the end. A file written by libopus decodes to
//!   exactly as many frames as were encoded.
//! - The output gain of the header is applied.
//! - Channels in the order WAV and FLAC use: mapping family 1, which holds
//!   up to eight channels in the order of the Vorbis I specification, is
//!   reordered as [`vorbis`](crate::vorbis) reorders it. Other families
//!   come in the order their mapping table gives.
//! - Samples are not clipped.
//!
//! # How close to the reference
//!
//! The SILK layer is decoded in fixed point, bit for bit as RFC 6716 and
//! its reference implementation do, and resampled to 48 kHz as the
//! reference does: a stream of SILK frames alone decodes to the samples
//! libopus gives, bit for bit. The CELT layer follows the floating-point
//! build of the reference implementation, operation for operation where
//! the order changes the result, but with an FFT of its own, so its
//! samples differ from the reference's by rounding: within 3e-7 of the
//! libopus FFmpeg 7.1 links, once `2^x` is approximated as that libopus,
//! built with `FLOAT_APPROX`, approximates it, and within 8e-5 (86 dB
//! below the signal) with the C library's `exp`, which this decoder uses,
//! as a default build of libopus does. `tests/opus.rs` holds the
//! comparison.
//!
//! # Limits
//!
//! - A packet that was lost, or one of a byte or none (the silence of
//!   discontinuous transmission), is decoded as silence: packet loss
//!   concealment, which RFC 6716 leaves to the decoder, is not done, and
//!   neither is the concealment the reference runs to smooth a change of
//!   mode the encoder sent no redundant frame for, which is replaced by a
//!   fade from silence. libopus sends one for every change of mode unless
//!   its bitrate is too low.
//! - A seek is exact, bit for bit what a decode from the start gives, when
//!   it lands within about 20 s after a place already decoded from the
//!   start; any other seek starts decoding 800 ms before its target, as
//!   RFC 7845 recommends, and is only close: Opus does not bound how long
//!   a decoder started elsewhere takes to become the same as one started
//!   at the beginning. See [`AudioReader::seek`](crate::AudioReader::seek).
//! - Only the first Opus stream of a file is read; chained streams are not.
//!
//! # Writing
//!
//! [`write`](fn@write) and [`AudioWriter::opus`](crate::AudioWriter::opus)
//! encode Ogg Opus at a constant or variable bitrate from 6 to 510 kbit/s
//! a channel, in frames of 2.5, 5, 10 or 20 ms ([`OpusOptions`]), with the
//! comments and pictures of a [`Metadata`]. The encoder codes the CELT
//! layer alone, in full band, the layer Opus has for music; SILK and
//! hybrid frames, which code speech in fewer bits, are not written.
//!
//! - Audio at any rate but 48 kHz is taken there first, with the
//!   windowed-sinc converter of `manguebyte-dsp` (kept in this crate,
//!   which depends on `manguebyte-core` alone); the identification header
//!   records the rate it came at.
//! - Each frame goes through the forward MDCT of the decoder's window, one
//!   long block or, where the energy of the input jumps or falls steeply,
//!   short blocks, recombined into the frequency resolution of a long one
//!   in the bands where that is sparser. The band energies are coded
//!   coarse, intra or predicted from the frame before, whichever is
//!   cheaper, and fine; the shapes with the pyramid vector quantiser,
//!   split and spread as RFC 6716 has it; peaks that stand out of the
//!   spectrum get more bits (dynalloc), and bands above the audio's own
//!   bandwidth or too poor to be worth coding are skipped.
//! - Two channels are coded as mid and side with the angle of each band,
//!   apart (dual stereo) when they are sparser so, and as one (intensity
//!   stereo) from a band the bitrate sets: none at 128 kbit/s and up. Up
//!   to eight channels go into the coupled pairs and single channels of
//!   the Vorbis layouts (mapping family 1), more into a stream each
//!   (family 255).
//! - The coding goes through the decoder's own allocation and band code,
//!   with an encoding range coder in place of the decoding one, so that
//!   the encoder holds the state the decoder will.
//! - The pre-skip is 120 samples, the overlap of the first block, and the
//!   granule position of the last page ends the stream where the audio
//!   did: it decodes to exactly the frames written, at 48 kHz.
//!
//! On the stereo test signal of this crate at 48 kHz, a constant
//! 64 kbit/s puts the noise 1.4 dB under the masking threshold of a model
//! apart from the encoder's, 128 kbit/s 8.7 dB and 256 kbit/s 15.7 dB, on
//! average over the bands; a steady chord comes back 46.7 dB above its
//! noise from 64 kbit/s up, the most the caps on each band's bits allow. A
//! variable bitrate asked for 128 kbit/s spends 91 on it, the narrow tones
//! and the silence taking less, for 9.4 dB under the threshold. On one
//! core, an optimised build encodes stereo at 48 kHz some 115 times faster
//! than real time, 5.1 some 60 times, and stereo at 44.1 kHz some 33
//! times, most of it spent taking the audio to 48 kHz; an unoptimised
//! build about a quarter as fast. Its memory does not grow with the
//! length of the stream; it allocates and does I/O, so it belongs off the
//! audio thread.
//!
//! It is a simple encoder next to libopus: no pitch pre-filter, no
//! spreading or allocation trim chosen from the signal, no analysis of
//! tonality or of the stereo image across frames, and the variable bitrate
//! follows simple rules rather than a model of the audio. The files it
//! writes have been decoded by this crate's decoder only.

use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Cursor, Read, Seek, Write};
use std::path::Path;

use manguebyte_core::AudioBuffer;

use crate::metadata::Metadata;

mod celt;
mod decoder;
mod encode;
mod packet;
mod range;
mod reader;
mod resample;
mod silk;

pub(crate) use encode::OpusStream;
pub use encode::{FrameDuration, OpusOptions};
pub(crate) use reader::OpusReader;

/// The rate every Opus stream decodes at.
pub const SAMPLE_RATE: u32 = 48_000;

/// The contents of an Ogg Opus file.
#[derive(Debug, Clone, PartialEq)]
pub struct OpusFile {
    /// The decoded audio, at 48 kHz.
    pub buffer: AudioBuffer<f32>,
    /// The rate of the audio that was encoded, as the header states it, for
    /// information; zero if it does not say.
    pub input_rate: u32,
    /// The name of the encoder, from the comment header.
    pub vendor: String,
    /// The comments, such as `TITLE` or `ARTIST`, in the order of the file.
    pub comments: Vec<(String, String)>,
    /// The same comments read into the fields of [`Metadata`].
    pub metadata: Metadata,
}

/// What went wrong while reading an Ogg Opus file.
#[derive(Debug)]
pub enum OpusError {
    /// The underlying reader failed.
    Io(io::Error),
    /// The data is not an Ogg stream that starts with Opus.
    NotOpus,
    /// The data ends before the headers do.
    Truncated,
    /// The stream breaks the rules of Ogg or of Opus.
    Malformed(&'static str),
    /// The stream is valid but uses something this decoder does not.
    Unsupported(&'static str),
    /// A page's checksum does not match its contents: the file is damaged.
    Corrupt,
}

impl fmt::Display for OpusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::NotOpus => f.write_str("not an Ogg Opus file"),
            Self::Truncated => f.write_str("Ogg Opus file ends unexpectedly"),
            Self::Malformed(what) => write!(f, "malformed Ogg Opus file: {what}"),
            Self::Unsupported(what) => write!(f, "unsupported Ogg Opus file: {what}"),
            Self::Corrupt => {
                f.write_str("Ogg Opus file is damaged: a page checksum does not match")
            }
        }
    }
}

impl std::error::Error for OpusError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for OpusError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Whether `bytes`, the start of an Ogg file, begin an Opus stream: its
/// first page holds the identification header.
pub(crate) fn starts_opus(bytes: &[u8]) -> bool {
    let Some(&segments) = bytes.get(26) else {
        return false;
    };
    let body = 27 + usize::from(segments);
    bytes.get(body..body + 8) == Some(b"OpusHead")
}

/// Reads an Ogg Opus file from `reader`. The whole stream is loaded into
/// memory.
pub fn read<R: Read>(mut reader: R) -> Result<OpusFile, OpusError> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    decode(&bytes)
}

/// Reads the Ogg Opus file at `path`.
pub fn read_file<P: AsRef<Path>>(path: P) -> Result<OpusFile, OpusError> {
    read(BufReader::new(File::open(path)?))
}

/// Writes `buffer` to `writer` as Ogg Opus, from audio at `sample_rate`
/// hertz, which is taken to 48 kHz if it is at another rate.
///
/// Samples are taken with full scale at `±1.0`; NaN becomes zero. The
/// encoder and what it makes are described in the
/// [module documentation](self#writing). To write block by block, use
/// [`AudioWriter::opus`](crate::AudioWriter::opus).
///
/// # Errors
///
/// [`OpusError::Unsupported`] for options that [`OpusOptions::validate`]
/// refuses, before anything is written, and [`OpusError::Io`] if `writer`
/// fails.
pub fn write<W: Write + Seek>(
    mut writer: W,
    buffer: &AudioBuffer<f32>,
    sample_rate: u32,
    options: &OpusOptions,
) -> Result<(), OpusError> {
    encode::write_buffer(&mut writer, buffer, sample_rate, options)
}

/// Writes `buffer` to a new Ogg Opus file at `path`, replacing any file
/// there. The options are checked before the file is created.
pub fn write_file<P: AsRef<Path>>(
    path: P,
    buffer: &AudioBuffer<f32>,
    sample_rate: u32,
    options: &OpusOptions,
) -> Result<(), OpusError> {
    options.validate(buffer.num_channels(), sample_rate)?;
    let mut file = BufWriter::new(File::create(path)?);
    encode::write_buffer(&mut file, buffer, sample_rate, options)?;
    file.flush()?;
    Ok(())
}

fn decode(bytes: &[u8]) -> Result<OpusFile, OpusError> {
    let mut reader = OpusReader::new(Cursor::new(bytes))?;
    let channels = reader.num_channels();
    // A byte holds at most 960 samples (a 20 ms frame in two bytes).
    let plausible = (bytes.len() as u64).saturating_mul(960);
    let expected = reader.frames().unwrap_or(0).min(plausible) as usize;
    let mut samples: Vec<Vec<f32>> = vec![Vec::with_capacity(expected); channels];
    let mut block = AudioBuffer::<f32>::new(channels, 5_760);
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
    Ok(OpusFile {
        buffer,
        input_rate: reader.input_rate(),
        vendor: comments.vendor,
        comments: comments.comments,
        metadata,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! case {
        ($name:literal) => {
            (
                $name,
                include_bytes!(concat!("../tests/data/", $name, ".opus")).as_slice(),
                include_bytes!(concat!("../tests/data/", $name, ".ref")).as_slice(),
            )
        };
    }

    /// With `2^x` worked out as the libopus the references come from works
    /// it out (`FLAG_APPROX`), every file decodes to within the rounding of
    /// single precision of libopus. The integration tests compare the
    /// decoder as it is, with the C library's `exp`.
    #[test]
    fn with_the_reference_s_approximation_the_decode_is_within_rounding() {
        celt::APPROXIMATE_EXP2.set(true);
        for (name, bytes, reference) in [
            case!("opus-celt-2ch-20ms-128k"),
            case!("opus-celt-2ch-2.5ms-96k"),
            case!("opus-celt-1ch-swb-10ms-40k"),
            case!("opus-celt-1ch-nb-5ms-24k"),
            case!("opus-celt-2ch-wb-40ms-cbr"),
            case!("opus-celt-2ch-40ms-vbr"),
            case!("opus-silk-2ch-wb-40ms"),
            case!("opus-hybrid-1ch-swb-20ms"),
            case!("opus-hybrid-2ch-fb-10ms"),
            case!("opus-audio-2ch-120ms"),
            case!("opus-audio-2ch-16k"),
            case!("opus-2ch-mono-stereo-splice"),
            case!("opus-6ch-family1"),
            case!("opus-2ch-family255"),
        ] {
            let file = read(bytes).unwrap();
            let word = |at: usize| u32::from_le_bytes(reference[at..at + 4].try_into().unwrap());
            let (step, channels) = (word(0) as usize, word(4) as usize);
            let frames = u64::from_le_bytes(reference[8..16].try_into().unwrap()) as usize;
            assert_eq!(file.buffer.num_frames(), frames, "{name}");
            let mut worst = 0.0f32;
            let mut signal = 0.0f64;
            let mut noise = 0.0f64;
            for (kept, frame) in (0..frames).step_by(step).enumerate() {
                for channel in 0..channels {
                    let at = 16 + 4 * (kept * channels + channel);
                    let theirs = f32::from_le_bytes(reference[at..at + 4].try_into().unwrap());
                    let ours = file.buffer.channel(channel)[frame];
                    worst = worst.max((ours - theirs).abs());
                    signal += f64::from(theirs).powi(2);
                    noise += f64::from(ours - theirs).powi(2);
                }
            }
            let snr = 10.0 * (signal / noise.max(1e-30)).log10();
            println!("{name}: {snr:.1} dB, largest difference {worst:.2e}");
            assert!(worst <= 2e-6, "{name}: {worst}");
            assert!(snr >= 125.0, "{name}: {snr:.1} dB");
        }
        celt::APPROXIMATE_EXP2.set(false);
    }
}
