//! MP3 and its elders: MPEG audio layers I, II and III.
//!
//! A decoder written from ISO/IEC 11172-3 (MPEG-1) and ISO/IEC 13818-3
//! (the lower sample frequencies of MPEG-2). Layer III has the further
//! halving of the sample frequencies known as MPEG-2.5: every sample rate
//! from 8 to 48 kHz, mono, stereo, dual channel and joint stereo with
//! mid/side and intensity coding, long, short and mixed blocks, the bit
//! reservoir, and the CRC that frames may carry. Layers I and II (`.mp1`,
//! `.mp2`, the audio of DVB, DAB and Video CD) come at 16 to 48 kHz, with
//! every allocation table, the grouping of three samples, the selection of
//! scale factors and joint stereo, and their CRC.
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
//! - ID3v2 tags at the start of a file, of versions 2.2, 2.3 and 2.4, and
//!   an ID3v1 tag at its end, are read into [`Mp3File::metadata`]. Anything
//!   between or after the frames that is not a frame, such as an APE tag,
//!   is stepped over.
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
//!   of the stream has been reached. Layers I and II have neither: their
//!   audio starts 481 samples in, the delay of the two filter banks, and
//!   ends where the last frame does.
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
//! - Free format, whose frames state no bitrate and which encoders do not
//!   write unless told to, is refused with [`Mp3Error::Unsupported`].
//! - A stream must keep one layer, one sample rate and one number of
//!   channels; frames of another kind are stepped over.
//! - The multichannel extension of 13818-3 to layers I and II is not read:
//!   such a stream decodes to the two channels it is compatible with.

use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::Path;

use manguebyte_core::AudioBuffer;

use crate::metadata::{Budget, MAX_CHUNK, Metadata, read_id3v1, read_id3v2};
use crate::pcm::read_up_to;

mod bits;
mod header;
mod huffman;
mod layer12;
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
    /// What the ID3v2 and ID3v1 tags say.
    pub metadata: Metadata,
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

/// Reads the ID3v2 tags at the start of a stream, and the ID3v1 tag at its
/// end, into one [`Metadata`]: where both say something, ID3v2 is taken.
/// A tag larger than 16 MiB is stepped over, and damage to a tag leaves
/// out what it damaged.
pub(crate) fn read_tags<R: Read + Seek>(reader: &mut R) -> io::Result<Metadata> {
    let len = reader.seek(SeekFrom::End(0))?;
    let mut metadata = Metadata::default();
    let mut budget = Budget::new();
    let mut start = 0u64;
    while start < len {
        reader.seek(SeekFrom::Start(start))?;
        let mut head = [0u8; 10];
        let have = read_up_to(reader, &mut head)?;
        let Some(tag_len) = id3v2_len(&head[..have]) else {
            break;
        };
        let keep = tag_len.min(len - start);
        if keep <= MAX_CHUNK {
            reader.seek(SeekFrom::Start(start))?;
            let mut tag = vec![0u8; keep as usize];
            reader.read_exact(&mut tag)?;
            read_id3v2(&tag, &mut metadata, &mut budget);
        }
        start = start.saturating_add(tag_len);
    }
    if len >= start.saturating_add(128) {
        reader.seek(SeekFrom::End(-128))?;
        let mut last = [0u8; 128];
        reader.read_exact(&mut last)?;
        if let Some(v1) = read_id3v1(&last) {
            metadata.fill_from(v1);
        }
    }
    Ok(metadata)
}

fn decode(bytes: &[u8]) -> Result<Mp3File, Mp3Error> {
    let metadata = read_tags(&mut Cursor::new(bytes))?;
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
        metadata,
    })
}
