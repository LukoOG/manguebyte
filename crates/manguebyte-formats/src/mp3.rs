//! MP3: MPEG audio layer III.
//!
//! A decoder written from ISO/IEC 11172-3 (MPEG-1) and ISO/IEC 13818-3
//! (the lower sample frequencies of MPEG-2), with the further halving of
//! the sample frequencies known as MPEG-2.5: every sample rate from 8 to
//! 48 kHz, mono, stereo, dual channel and joint stereo with mid/side and
//! intensity coding, long, short and mixed blocks, the bit reservoir, and
//! the CRC that frames may carry.
//!
//! ```no_run
//! let file = manguebyte_formats::mp3::read_file("track.mp3")?;
//! println!("{} Hz, {} channels, {} frames",
//!     file.sample_rate, file.buffer.num_channels(), file.buffer.num_frames());
//! # Ok::<(), manguebyte_formats::mp3::Mp3Error>(())
//! ```
//!
//! The functions here hold a whole file in memory. To decode block by block
//! and to seek, use [`AudioReader`](crate::AudioReader).
//!
//! # What comes out
//!
//! - ID3v2 tags at the start of a file are stepped over, and so is anything
//!   between or after the frames that is not a frame, such as an ID3v1 or
//!   APE tag at the end.
//! - Many encoders put a frame of silence first that holds a Xing, Info or
//!   VBRI header, which says how many frames follow. That frame is not
//!   audio and is not decoded. When it also holds a LAME tag, which LAME and
//!   FFmpeg write, the tag says how many samples the encoder added before
//!   and after the audio: those are removed, together with the 529 samples
//!   the decoder's filter banks take to give their first one, so that the
//!   decoded audio has exactly the length and the timing of what was
//!   encoded.
//! - Without a LAME tag, every decoded sample is given, delay included, and
//!   without a Xing or VBRI header the length is known only once the end
//!   of the stream has been reached.
//! - Samples are not clipped: a lossy codec does not give back the samples
//!   it was given, and loud audio can come back slightly beyond full scale.
//!
//! # Damage
//!
//! A frame that carries a CRC is checked against it, and one that does not
//! match is reported as an error, as is side information that breaks the
//! rules of the format. Most frames carry no CRC, and damage to their audio
//! cannot be seen: it is decoded as it is. Where something that is not a
//! frame stands where a frame should, the decoder looks for the next frame,
//! which must be followed by another, as a decoder of a broadcast does.
//!
//! # Limits
//!
//! - Layers I and II are recognised and refused with
//!   [`Mp3Error::Unsupported`]; so is free format, whose frames state no
//!   bitrate, which encoders do not write unless told to.
//! - A stream must keep one sample rate and one number of channels; frames
//!   of another kind are stepped over.

use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, Cursor, Read};
use std::path::Path;

use manguebyte_core::AudioBuffer;

mod bits;
mod header;
mod huffman;
mod layer3;
mod reader;
mod synth;
mod tables;

pub(crate) use header::{FrameHeader, id3v2_len};
pub(crate) use reader::Mp3Reader;

/// The contents of an MP3 file.
#[derive(Debug, Clone, PartialEq)]
pub struct Mp3File {
    /// The decoded audio.
    pub buffer: AudioBuffer<f32>,
    /// Sample rate in hertz.
    pub sample_rate: u32,
}

/// What went wrong while reading an MP3 file.
#[derive(Debug)]
pub enum Mp3Error {
    /// The underlying reader failed.
    Io(io::Error),
    /// The data holds no MPEG audio frame.
    NotMp3,
    /// The stream breaks the rules of the format.
    Malformed(&'static str),
    /// The stream is valid but uses something this decoder does not.
    Unsupported(&'static str),
    /// A frame's CRC does not match its contents: the file is damaged.
    Corrupt,
}

impl fmt::Display for Mp3Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::NotMp3 => f.write_str("not an MP3 file"),
            Self::Malformed(what) => write!(f, "malformed MP3 file: {what}"),
            Self::Unsupported(what) => write!(f, "unsupported MP3 file: {what}"),
            Self::Corrupt => f.write_str("MP3 file is damaged: a frame's CRC does not match"),
        }
    }
}

impl std::error::Error for Mp3Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for Mp3Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Reads an MP3 file from `reader`. The whole stream is loaded into memory.
pub fn read<R: Read>(mut reader: R) -> Result<Mp3File, Mp3Error> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    decode(&bytes)
}

/// Reads the MP3 file at `path`.
pub fn read_file<P: AsRef<Path>>(path: P) -> Result<Mp3File, Mp3Error> {
    read(BufReader::new(File::open(path)?))
}

fn decode(bytes: &[u8]) -> Result<Mp3File, Mp3Error> {
    let mut reader = Mp3Reader::new(Cursor::new(bytes))?;
    let channels = reader.num_channels();
    // The length a header states is trusted only as far as the data could
    // hold it: a frame of the lowest bitrate holds 1152 samples in 96 bytes
    // in MPEG-1, 576 in 24 bytes at the lower rates.
    let per_byte = if reader.sample_rate() >= 32_000 {
        12
    } else {
        24
    };
    let plausible = (bytes.len() as u64).saturating_mul(per_byte);
    let expected = reader.frames().unwrap_or(0).min(plausible) as usize;
    let mut samples: Vec<Vec<f32>> = vec![Vec::with_capacity(expected); channels];
    let mut block = AudioBuffer::<f32>::new(channels, 4_608);
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
    Ok(Mp3File {
        buffer,
        sample_rate: reader.sample_rate(),
    })
}
