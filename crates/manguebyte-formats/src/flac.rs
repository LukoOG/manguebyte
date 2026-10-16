//! FLAC, the Free Lossless Audio Codec.
//!
//! An encoder and a decoder written from the format's specification,
//! RFC 9639.
//!
//! # Decoding
//!
//! The decoder reads every kind of frame the format defines: constant,
//! verbatim, fixed and linear-prediction subframes; both Rice codings with
//! escapes; wasted bits; all three stereo decorrelations; 4 to 32 bits per
//! sample; up to eight channels. The checksums on every frame are verified,
//! and a frame that passes them and still decodes to samples outside the
//! stream's bit depth is refused as malformed: its predictor was unstable,
//! which no encoder writes and a checksum cannot see.
//!
//! ```no_run
//! let file = manguebyte_formats::flac::read_file("song.flac")?;
//! println!("{} Hz, {} channels, {} frames",
//!     file.sample_rate, file.buffer.num_channels(), file.buffer.num_frames());
//! # Ok::<(), manguebyte_formats::flac::FlacError>(())
//! ```
//!
//! The functions here hold a whole file in memory. To decode block by block
//! and to seek, use [`AudioReader`](crate::AudioReader).
//!
//! # Encoding
//!
//! [`write()`] encodes a whole buffer, [`write_integers`] integer samples of
//! any depth up to 32 bits, and [`AudioWriter::flac`](crate::AudioWriter::flac)
//! a stream of any length a block at a time, producing the same bytes.
//! [`FlacOptions`] sets the bit depth (4 to 32), the compression level (0 to
//! 8), the block size, padding, seek points and tags. One to eight channels
//! and any sample rate the format can state are accepted.
//! [`FlacOptions::validate`] checks options against these
//! [limits](FlacOptions#limits) without writing anything.
//!
//! Each channel of each block is coded in the smallest of the ways the
//! format offers, as far as the level searches: one value for a block that
//! does not change; plain samples, so that noise never grows by more than
//! the frame headers; fixed predictors of order 0 to 4; and from level 3,
//! linear predictors of up to order 12, from the autocorrelation of
//! the windowed block and the Levinson-Durbin recursion, with quantised
//! coefficients, trying several orders, precisions and windows. Low bits
//! that are zero throughout a block are not stored. Two channels are coded
//! as left and right, left and side, side and right, or mid and side. At
//! levels 2 and 5 to 8 all four are coded and the smallest is kept; at the
//! others one is chosen from an estimate, which is faster and does not
//! always pick the smallest. Level 1 differs from level 2 in nothing else:
//! on the music measured below it comes out 12 bytes larger, and on two
//! random walks made so that the choice is often close, 390 bytes or 0.3 %
//! larger, having missed the smallest in 16 frames of 40.
//! Residuals are Rice coded, with the number of
//! partitions, the parameter of each, the parameter width and escaped
//! partitions all chosen by counting bits. Arithmetic is 64-bit throughout,
//! and residuals that would reach 2^31, which the format forbids, rule a
//! predictor out, so 24-bit and 32-bit audio and their 33-bit side channels
//! cannot overflow.
//!
//! The stream information carries the MD5 digest of the audio (computed by
//! this crate, from RFC 1321), the sizes of the smallest and largest frame
//! and the length. A seek table and a Vorbis comment with the encoder's name
//! and the caller's tags follow, then padding.
//!
//! Sizes, measured on five seconds of synthetic music at 44.1 kHz in 16-bit
//! stereo (882,000 bytes of samples: a plucked melody, bass, a chord, drums,
//! echoes and a noise floor; `music` in `tests/flac_encoder.rs`), against
//! FFmpeg 7.1's encoder at the same level. Bytes of audio frames, metadata
//! left out:
//!
//! | Level | This encoder | FFmpeg 7.1 | Ratio |
//! |---|---|---|---|
//! | 0 | 533,210 (60.5 %) | 568,369 (64.4 %) | 0.938 |
//! | 5 | 492,265 (55.8 %) | 492,876 (55.9 %) | 0.999 |
//! | 8 | 482,807 (54.7 %) | 490,211 (55.6 %) | 0.985 |
//!
//! FFmpeg's highest level, 12, gives 487,535. A test holds the three ratios
//! below 1.03. One kind of material is not a benchmark: other audio may
//! rank the two encoders differently.
//!
//! White noise is stored as plain samples and takes the size of the samples
//! plus, per frame, at most 16 bytes of header, a byte of subframe header
//! per channel and 2 bytes of checksum.
//!
//! What the tests write was also decoded by FFmpeg 7.1, by hand: every
//! level, 4 to 32 bits, 1 to 8 channels, block sizes from 16 to 65535, rates
//! from 1 Hz to 1,048,575 Hz. FFmpeg gave back the same samples, bit for
//! bit, without an error, and they match the MD5 digest the encoder wrote.
//! The exception is frames larger than 208,012 bytes, which only blocks far
//! beyond the streamable subset can make: from those FFmpeg returns no
//! samples and no error. This crate's decoder reads them. See
//! `tests/data/README.md`.

use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::Path;

use manguebyte_core::AudioBuffer;

use crate::pcm::read_up_to;
use crate::window::Window;

mod bits;
mod encode;
mod frame;
mod lpc;
mod md5;
mod rice;

pub(crate) use encode::FlacStream;
pub use encode::{FlacOptions, write, write_file, write_integers};

/// The contents of a FLAC file.
#[derive(Debug, Clone, PartialEq)]
pub struct FlacFile {
    /// The audio, converted to `f32` with integer full scale mapped to
    /// `±1.0`.
    pub buffer: AudioBuffer<f32>,
    /// Sample rate in hertz.
    pub sample_rate: u32,
    /// Bit depth the samples were stored with.
    pub bits_per_sample: u16,
}

/// What went wrong while reading or writing a FLAC file.
#[derive(Debug)]
pub enum FlacError {
    /// The underlying reader or writer failed, or, with kind
    /// [`InvalidInput`](io::ErrorKind::InvalidInput), what was to be written
    /// is outside what FLAC or this encoder can hold: see the
    /// [limits](FlacOptions#limits).
    ///
    /// A reader or writer can fail with kind
    /// [`InvalidInput`](io::ErrorKind::InvalidInput) as well. To tell a
    /// mistake in the options from a failing destination, check them first
    /// with [`FlacOptions::validate`].
    Io(io::Error),
    /// The data does not start with the FLAC marker.
    NotFlac,
    /// The data ends in the middle of something.
    Truncated,
    /// The file breaks the format's rules.
    Malformed(&'static str),
    /// A frame's checksum does not match its contents: the file is damaged.
    Corrupt,
}

impl fmt::Display for FlacError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::NotFlac => f.write_str("not a FLAC file"),
            Self::Truncated => f.write_str("FLAC file ends unexpectedly"),
            Self::Malformed(what) => write!(f, "malformed FLAC file: {what}"),
            Self::Corrupt => f.write_str("FLAC file is damaged: a checksum does not match"),
        }
    }
}

impl std::error::Error for FlacError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for FlacError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Reads a FLAC file from `reader`. The whole stream is loaded into memory.
pub fn read<R: Read>(mut reader: R) -> Result<FlacFile, FlacError> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    decode(&bytes)
}

/// Reads the FLAC file at `path`.
pub fn read_file<P: AsRef<Path>>(path: P) -> Result<FlacFile, FlacError> {
    read(BufReader::new(File::open(path)?))
}

/// Reads bits from a byte slice, most significant bit first.
struct BitReader<'a> {
    bytes: &'a [u8],
    /// Position in bits from the start of `bytes`.
    position: usize,
}

impl<'a> BitReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn byte_position(&self) -> usize {
        self.position / 8
    }

    #[cfg(test)]
    fn is_aligned(&self) -> bool {
        self.position % 8 == 0
    }

    fn align(&mut self) {
        self.position = self.position.div_ceil(8) * 8;
    }

    fn at_end(&self) -> bool {
        self.byte_position() >= self.bytes.len()
    }

    /// Reads up to 64 bits as an unsigned number.
    fn bits(&mut self, count: u32) -> Result<u64, FlacError> {
        debug_assert!(count <= 64);
        if self.position + count as usize > self.bytes.len() * 8 {
            return Err(FlacError::Truncated);
        }
        let mut value = 0u64;
        let mut left = count;
        while left > 0 {
            let byte = self.bytes[self.position / 8];
            let offset = (self.position % 8) as u32;
            let take = left.min(8 - offset);
            let chunk = (byte >> (8 - offset - take)) & (((1u16 << take) - 1) as u8);
            value = (value << take) | u64::from(chunk);
            self.position += take as usize;
            left -= take;
        }
        Ok(value)
    }

    /// Reads a two's-complement number of `count` bits.
    fn signed(&mut self, count: u32) -> Result<i64, FlacError> {
        if count == 0 {
            return Ok(0);
        }
        let raw = self.bits(count)?;
        let shift = 64 - count;
        Ok(((raw << shift) as i64) >> shift)
    }

    /// Counts zero bits up to the next one bit, and consumes that one.
    ///
    /// The count is 64 bits wide so that no run of zeros a slice can hold
    /// overflows it.
    fn unary(&mut self) -> Result<u64, FlacError> {
        let mut zeros = 0u64;
        loop {
            if self.position >= self.bytes.len() * 8 {
                return Err(FlacError::Truncated);
            }
            // Whole zero bytes can be skipped at once.
            let byte = self.bytes[self.position / 8];
            let offset = (self.position % 8) as u32;
            let rest = byte << offset;
            if rest == 0 {
                zeros += u64::from(8 - offset);
                self.position += (8 - offset) as usize;
                continue;
            }
            let leading = rest.leading_zeros();
            zeros += u64::from(leading);
            self.position += leading as usize + 1;
            return Ok(zeros);
        }
    }
}

/// One step of each checksum for every possible byte, worked out when the
/// crate is compiled. Checking a frame is then one lookup per byte rather
/// than eight shifts.
const CRC8_TABLE: [u8; 256] = {
    let mut table = [0u8; 256];
    let mut byte = 0;
    while byte < 256 {
        let mut crc = byte as u8;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
            bit += 1;
        }
        table[byte] = crc;
        byte += 1;
    }
    table
};

const CRC16_TABLE: [u16; 256] = {
    let mut table = [0u16; 256];
    let mut byte = 0;
    while byte < 256 {
        let mut crc = (byte as u16) << 8;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
            bit += 1;
        }
        table[byte] = crc;
        byte += 1;
    }
    table
};

/// CRC-8 with polynomial x^8 + x^2 + x + 1, as used on frame headers.
fn crc8(bytes: &[u8]) -> u8 {
    bytes
        .iter()
        .fold(0, |crc, &byte| CRC8_TABLE[usize::from(crc ^ byte)])
}

/// CRC-16 with polynomial x^16 + x^15 + x^2 + 1, as used on whole frames.
fn crc16(bytes: &[u8]) -> u16 {
    bytes.iter().fold(0, |crc, &byte| {
        (crc << 8) ^ CRC16_TABLE[usize::from((crc >> 8) as u8 ^ byte)]
    })
}

/// What the stream's first metadata block says about it.
#[derive(Debug, Clone, Copy)]
struct StreamInfo {
    /// Samples in the largest block, as the encoder declared it.
    max_block_size: u32,
    /// Bytes in the largest frame, or zero if the encoder did not say.
    max_frame_size: u32,
    sample_rate: u32,
    channels: usize,
    bits_per_sample: u32,
    total_frames: u64,
}

/// A frame the seek table says starts at a known place.
#[derive(Debug, Clone, Copy)]
struct SeekPoint {
    /// Number of the frame's first sample.
    sample: u64,
    /// Where the frame starts, counted from the first frame.
    offset: u64,
}

/// The sample number a seek point carries when it is only reserved space.
const PLACEHOLDER_POINT: u64 = u64::MAX;

/// How many seek points are kept. A table may hold close to a million; a
/// point every few seconds is all that seeking needs, and whatever lies past
/// the last point kept is found by bisection.
const MAX_SEEK_POINTS: usize = 32_768;

/// What comes before the audio of a FLAC stream.
struct Metadata {
    info: StreamInfo,
    /// Offset of the first frame.
    audio_start: u64,
    /// Length of the whole stream in bytes.
    stream_len: u64,
    seek_points: Vec<SeekPoint>,
}

/// Reads the marker and the metadata blocks, leaving the audio unread.
///
/// The seek table is skipped unless `with_seek_table` is set.
fn read_metadata<R: Read + Seek>(
    reader: &mut R,
    with_seek_table: bool,
) -> Result<Metadata, FlacError> {
    let stream_len = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(0))?;
    let mut marker = [0u8; 4];
    if stream_len < 4 {
        return Err(FlacError::NotFlac);
    }
    reader.read_exact(&mut marker)?;
    if &marker != b"fLaC" {
        return Err(FlacError::NotFlac);
    }

    let mut position = 4u64;
    let mut info = None;
    let mut seek_points = Vec::new();
    loop {
        if position + 4 > stream_len {
            return Err(FlacError::Truncated);
        }
        reader.seek(SeekFrom::Start(position))?;
        let mut header = [0u8; 4];
        reader.read_exact(&mut header)?;
        let is_last = header[0] & 0x80 != 0;
        let kind = header[0] & 0x7F;
        let length = u64::from(u32::from_be_bytes([0, header[1], header[2], header[3]]));
        if position + 4 + length > stream_len {
            return Err(FlacError::Truncated);
        }
        match kind {
            0 => {
                // Only the first 34 bytes are defined.
                let mut body = [0u8; 34];
                let have = length.min(34) as usize;
                reader.read_exact(&mut body[..have])?;
                info = Some(stream_info(&body[..have])?);
            }
            3 if with_seek_table => {
                seek_points.clear();
                read_seek_points(reader, length, &mut seek_points)?;
            }
            _ => {}
        }
        position += 4 + length;
        if is_last {
            break;
        }
    }
    let info = info.ok_or(FlacError::Malformed("no stream information block"))?;
    Ok(Metadata {
        info,
        audio_start: position,
        stream_len,
        seek_points,
    })
}

/// Reads a seek table of `length` bytes, keeping the points that can be used:
/// real ones, in rising order.
fn read_seek_points<R: Read>(
    reader: &mut R,
    length: u64,
    points: &mut Vec<SeekPoint>,
) -> Result<(), FlacError> {
    // Each point is a sample number, an offset and a length: 18 bytes.
    const POINT: usize = 18;
    let mut chunk = [0u8; POINT * 64];
    let mut left = length / POINT as u64;
    // The caller has checked that the table lies within the file, so this
    // is no more than the file can back up, and no more than is kept.
    points.reserve_exact(left.min(MAX_SEEK_POINTS as u64) as usize);
    while left > 0 && points.len() < MAX_SEEK_POINTS {
        let count = left.min(64) as usize;
        let bytes = &mut chunk[..count * POINT];
        reader.read_exact(bytes)?;
        for point in bytes.chunks_exact(POINT) {
            let sample = u64::from_be_bytes([
                point[0], point[1], point[2], point[3], point[4], point[5], point[6], point[7],
            ]);
            let offset = u64::from_be_bytes([
                point[8], point[9], point[10], point[11], point[12], point[13], point[14],
                point[15],
            ]);
            let in_order = points
                .last()
                .is_none_or(|last: &SeekPoint| sample > last.sample && offset > last.offset);
            if sample != PLACEHOLDER_POINT && in_order && points.len() < MAX_SEEK_POINTS {
                points.push(SeekPoint { sample, offset });
            }
        }
        left -= count as u64;
    }
    Ok(())
}

fn decode(bytes: &[u8]) -> Result<FlacFile, FlacError> {
    let metadata = read_metadata(&mut Cursor::new(bytes), false)?;
    let info = metadata.info;
    // The metadata was read from `bytes`, so its end lies within them.
    let audio = &bytes[metadata.audio_start as usize..];

    // The length is known up front in almost every file. It is trusted only
    // up to what the data could possibly hold, so that a damaged header
    // cannot ask for an enormous allocation.
    let plausible = (bytes.len() as u64).saturating_mul(64);
    let expected = info.total_frames.min(plausible) as usize;
    let mut channels: Vec<Vec<i32>> = vec![Vec::with_capacity(expected); info.channels];

    let mut reader = BitReader::new(audio);
    let mut frame = FrameDecoder::new();
    while !reader.at_end() {
        frame.decode(&mut reader, &info)?;
        for (out, samples) in channels.iter_mut().zip(&frame.block) {
            out.extend(samples.iter().map(|&sample| sample as i32));
        }
    }

    let frames = channels.first().map_or(0, Vec::len);
    let scale = info.scale();
    let mut buffer = AudioBuffer::new(info.channels, frames);
    for (index, samples) in channels.iter().enumerate() {
        for (out, &sample) in buffer.channel_mut(index).iter_mut().zip(samples) {
            *out = (f64::from(sample) * scale) as f32;
        }
    }
    Ok(FlacFile {
        buffer,
        sample_rate: info.sample_rate,
        bits_per_sample: info.bits_per_sample as u16,
    })
}

impl StreamInfo {
    /// What an integer sample is multiplied by to map full scale to `±1.0`.
    fn scale(&self) -> f64 {
        1.0 / (1u64 << (self.bits_per_sample - 1)) as f64
    }
}

fn stream_info(body: &[u8]) -> Result<StreamInfo, FlacError> {
    if body.len() < 34 {
        return Err(FlacError::Malformed("stream information block too short"));
    }
    let max_block_size = u32::from(u16::from_be_bytes([body[2], body[3]]));
    let max_frame_size = u32::from_be_bytes([0, body[7], body[8], body[9]]);
    let mut reader = BitReader::new(&body[10..]);
    let sample_rate = reader.bits(20)? as u32;
    let channels = reader.bits(3)? as usize + 1;
    let bits_per_sample = reader.bits(5)? as u32 + 1;
    let total_frames = reader.bits(36)?;
    if sample_rate == 0 {
        return Err(FlacError::Malformed("sample rate of zero"));
    }
    if bits_per_sample < 4 {
        return Err(FlacError::Malformed("fewer than four bits per sample"));
    }
    Ok(StreamInfo {
        max_block_size,
        max_frame_size,
        sample_rate,
        channels,
        bits_per_sample,
        total_frames,
    })
}

/// How the channels of a frame were combined before coding.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Stereo {
    Independent,
    LeftSide,
    SideRight,
    MidSide,
}

/// What the header of a frame says about it.
#[derive(Debug, Clone, Copy)]
struct FrameHeader {
    /// Whether frames are numbered by their first sample, as in a stream
    /// whose blocks vary in size, or by their index.
    variable_blocks: bool,
    /// The number the frame carries: a sample number or a frame index.
    number: u64,
    /// Samples in each channel of the frame.
    block_size: usize,
    channels: usize,
    stereo: Stereo,
    bits_per_sample: u32,
}

/// The longest a frame header can be: the sync code and the codes, a number
/// of seven bytes, a block size and a sample rate of two bytes each, and the
/// checksum.
const MAX_FRAME_HEADER: usize = 16;

/// Reads a frame header, up to and including its checksum.
fn frame_header(reader: &mut BitReader<'_>, info: &StreamInfo) -> Result<FrameHeader, FlacError> {
    let frame_start = reader.byte_position();
    if reader.bits(15)? != 0b111_1111_1111_1100 {
        return Err(FlacError::Malformed(
            "frame does not start with the sync code",
        ));
    }
    let variable_blocks = reader.bits(1)? == 1;
    let block_code = reader.bits(4)?;
    let rate_code = reader.bits(4)?;
    let channel_code = reader.bits(4)?;
    let size_code = reader.bits(3)?;
    if reader.bits(1)? != 0 {
        return Err(FlacError::Malformed("reserved bit set in frame header"));
    }

    // The frame or sample number, coded like UTF-8 but up to 36 bits.
    let first = reader.bits(8)? as u8;
    let extra = match first.leading_ones() {
        0 => 0,
        ones @ 2..=7 => ones - 1,
        _ => return Err(FlacError::Malformed("invalid frame number")),
    };
    if !variable_blocks && extra > 5 {
        return Err(FlacError::Malformed("invalid frame number"));
    }
    // What is left of the first byte after its run of ones and a zero.
    let payload = if extra == 0 { 0x7F } else { 0x3F >> extra };
    let mut number = u64::from(first & payload);
    for _ in 0..extra {
        let byte = reader.bits(8)?;
        if byte & 0xC0 != 0x80 {
            return Err(FlacError::Malformed("invalid frame number"));
        }
        number = (number << 6) | (byte & 0x3F);
    }

    let block_size = match block_code {
        0 => return Err(FlacError::Malformed("reserved block size")),
        1 => 192,
        code @ 2..=5 => 576usize << (code - 2),
        6 => reader.bits(8)? as usize + 1,
        7 => reader.bits(16)? as usize + 1,
        code => 256usize << (code - 8),
    };
    match rate_code {
        12 => {
            reader.bits(8)?;
        }
        13 | 14 => {
            reader.bits(16)?;
        }
        15 => return Err(FlacError::Malformed("invalid sample rate code")),
        _ => {}
    }
    let bits_per_sample = match size_code {
        0 => info.bits_per_sample,
        1 => 8,
        2 => 12,
        4 => 16,
        5 => 20,
        6 => 24,
        7 => 32,
        _ => return Err(FlacError::Malformed("reserved sample size")),
    };
    let (channels, stereo) = match channel_code {
        code @ 0..=7 => (code as usize + 1, Stereo::Independent),
        8 => (2, Stereo::LeftSide),
        9 => (2, Stereo::SideRight),
        10 => (2, Stereo::MidSide),
        _ => return Err(FlacError::Malformed("reserved channel assignment")),
    };
    if channels != info.channels || bits_per_sample != info.bits_per_sample {
        return Err(FlacError::Malformed(
            "frame disagrees with the stream information",
        ));
    }

    let header_end = reader.byte_position();
    let header_crc = reader.bits(8)? as u8;
    if crc8(&reader.bytes[frame_start..header_end]) != header_crc {
        return Err(FlacError::Corrupt);
    }
    Ok(FrameHeader {
        variable_blocks,
        number,
        block_size,
        channels,
        stereo,
        bits_per_sample,
    })
}

/// Decodes one frame at a time into storage it keeps between frames.
struct FrameDecoder {
    /// The samples of the frame decoded last, one list per channel.
    block: [Vec<i64>; 8],
    /// How many times a frame was decoded or tried, for the tests that
    /// measure what a seek costs.
    #[cfg(test)]
    attempts: u64,
}

impl FrameDecoder {
    fn new() -> Self {
        Self {
            block: Default::default(),
            #[cfg(test)]
            attempts: 0,
        }
    }

    /// Decodes the frame `reader` is positioned at into `self.block`, and
    /// leaves `reader` at the first byte after it.
    fn decode(
        &mut self,
        reader: &mut BitReader<'_>,
        info: &StreamInfo,
    ) -> Result<FrameHeader, FlacError> {
        #[cfg(test)]
        {
            self.attempts += 1;
        }
        let frame_start = reader.byte_position();
        let header = frame_header(reader, info)?;

        for (index, samples) in self.block[..header.channels].iter_mut().enumerate() {
            // The side channel is a difference, so it needs one more bit.
            let is_side = matches!(
                (header.stereo, index),
                (Stereo::LeftSide, 1) | (Stereo::SideRight, 0) | (Stereo::MidSide, 1)
            );
            decode_subframe(
                reader,
                header.block_size,
                header.bits_per_sample + u32::from(is_side),
                samples,
            )?;
        }

        reader.align();
        let frame_end = reader.byte_position();
        let frame_crc = reader.bits(16)? as u16;
        if crc16(&reader.bytes[frame_start..frame_end]) != frame_crc {
            return Err(FlacError::Corrupt);
        }

        // A predictor that runs away gives numbers of any size, and matching
        // checksums do not rule one out: they only say the bytes are the
        // ones that were written. So the channels are combined in arithmetic
        // that wraps, as the predictors work, and the result is checked
        // below.
        let (first, rest) = self.block.split_at_mut(1);
        let (a, b) = (&mut first[0], &mut rest[0]);
        match header.stereo {
            Stereo::Independent => {}
            Stereo::LeftSide => {
                for (left, side) in a.iter().zip(b.iter_mut()) {
                    *side = left.wrapping_sub(*side);
                }
            }
            Stereo::SideRight => {
                for (side, right) in a.iter_mut().zip(b.iter()) {
                    *side = side.wrapping_add(*right);
                }
            }
            Stereo::MidSide => {
                for (mid, side) in a.iter_mut().zip(b.iter_mut()) {
                    // The mid channel lost its lowest bit when it was halved;
                    // it is the same as the side channel's.
                    let sum = (*mid << 1) | (*side & 1);
                    let difference = *side;
                    *mid = sum.wrapping_add(difference) >> 1;
                    *side = sum.wrapping_sub(difference) >> 1;
                }
            }
        }

        // A lossless coder gives back the samples it was given, which fit
        // the stream's bit depth. Anything else was not written by an
        // encoder, and is refused here rather than cut down to size and
        // played as noise.
        let limit = 1i64 << (header.bits_per_sample - 1);
        let fits = self.block[..header.channels].iter().all(|samples| {
            samples
                .iter()
                .all(|sample| (-limit..limit).contains(sample))
        });
        if !fits {
            return Err(FlacError::Malformed(
                "sample does not fit the stream's bit depth",
            ));
        }
        Ok(header)
    }
}

/// Decodes one channel of a frame into `samples`, replacing what it held.
fn decode_subframe(
    reader: &mut BitReader<'_>,
    block_size: usize,
    bits_per_sample: u32,
    samples: &mut Vec<i64>,
) -> Result<(), FlacError> {
    if reader.bits(1)? != 0 {
        return Err(FlacError::Malformed("padding bit set in subframe header"));
    }
    let kind = reader.bits(6)? as u32;
    let wasted = if reader.bits(1)? == 1 {
        reader.unary()? + 1
    } else {
        0
    };
    if wasted >= u64::from(bits_per_sample) {
        return Err(FlacError::Malformed("more wasted bits than bits"));
    }
    // Fewer than the bits of a sample, so it fits.
    let wasted = wasted as u32;
    let bits = bits_per_sample - wasted;

    samples.clear();
    samples.reserve(block_size);
    match kind {
        0 => {
            let value = reader.signed(bits)?;
            samples.resize(block_size, value);
        }
        1 => {
            for _ in 0..block_size {
                samples.push(reader.signed(bits)?);
            }
        }
        8..=12 => {
            let order = (kind - 8) as usize;
            if order > block_size {
                return Err(FlacError::Malformed("predictor order exceeds block size"));
            }
            for _ in 0..order {
                samples.push(reader.signed(bits)?);
            }
            decode_residual(reader, block_size, order, samples)?;
            const FIXED: [&[i64]; 5] = [&[], &[1], &[2, -1], &[3, -3, 1], &[4, -6, 4, -1]];
            predict(samples, FIXED[order], 0);
        }
        32..=63 => {
            let order = (kind - 31) as usize;
            if order > block_size {
                return Err(FlacError::Malformed("predictor order exceeds block size"));
            }
            for _ in 0..order {
                samples.push(reader.signed(bits)?);
            }
            let precision = reader.bits(4)? as u32 + 1;
            if precision == 16 {
                return Err(FlacError::Malformed("invalid predictor precision"));
            }
            let shift = reader.signed(5)?;
            if shift < 0 {
                return Err(FlacError::Malformed("negative predictor shift"));
            }
            let mut coefficients = [0i64; 32];
            for coefficient in coefficients.iter_mut().take(order) {
                *coefficient = reader.signed(precision)?;
            }
            decode_residual(reader, block_size, order, samples)?;
            predict(samples, &coefficients[..order], shift as u32);
        }
        _ => return Err(FlacError::Malformed("reserved subframe type")),
    }

    if wasted > 0 {
        for sample in samples.iter_mut() {
            *sample <<= wasted;
        }
    }
    Ok(())
}

/// Turns residuals into samples in place: each sample is its residual plus
/// a weighted sum of the samples before it.
fn predict(samples: &mut [i64], coefficients: &[i64], shift: u32) {
    let order = coefficients.len();
    for index in order..samples.len() {
        let prediction: i64 = coefficients
            .iter()
            .enumerate()
            .map(|(back, &coefficient)| coefficient.wrapping_mul(samples[index - 1 - back]))
            .fold(0i64, i64::wrapping_add);
        samples[index] = samples[index].wrapping_add(prediction >> shift);
    }
}

/// Reads the Rice-coded residuals of a subframe and appends them.
fn decode_residual(
    reader: &mut BitReader<'_>,
    block_size: usize,
    order: usize,
    samples: &mut Vec<i64>,
) -> Result<(), FlacError> {
    let parameter_bits = match reader.bits(2)? {
        0 => 4,
        1 => 5,
        _ => return Err(FlacError::Malformed("reserved residual coding")),
    };
    let escape = (1u64 << parameter_bits) - 1;
    let partition_order = reader.bits(4)? as u32;
    let partitions = 1usize << partition_order;
    if block_size % partitions != 0 || (block_size >> partition_order) < order {
        return Err(FlacError::Malformed("invalid residual partitioning"));
    }

    for partition in 0..partitions {
        let mut count = block_size >> partition_order;
        if partition == 0 {
            count -= order;
        }
        let parameter = reader.bits(parameter_bits)?;
        if parameter == escape {
            let raw_bits = reader.bits(5)? as u32;
            for _ in 0..count {
                samples.push(reader.signed(raw_bits)?);
            }
        } else {
            let parameter = parameter as u32;
            for _ in 0..count {
                let quotient = reader.unary()?;
                let folded = (quotient << parameter) | reader.bits(parameter)?;
                // Even numbers are positive, odd ones negative.
                samples.push((folded >> 1) as i64 ^ -((folded & 1) as i64));
            }
        }
    }
    Ok(())
}

/// A guess at the size of a frame, used until the stream has shown how large
/// its frames are.
const FIRST_FRAME_GUESS: usize = 16 * 1024;

/// How the frames of a stream are numbered, learned from its first frame.
#[derive(Debug, Clone, Copy)]
struct Numbering {
    /// Whether frames carry the number of their first sample rather than
    /// their own index.
    variable: bool,
    /// The number the first frame carries. Zero in a complete stream.
    base: u64,
    /// Samples in every frame but the last, when frames carry their index.
    block_size: u64,
}

impl Numbering {
    /// The position in the stream of the first sample of a frame.
    fn sample_of(&self, header: &FrameHeader) -> Option<u64> {
        if header.variable_blocks != self.variable {
            return None;
        }
        let number = header.number.checked_sub(self.base)?;
        if self.variable {
            Some(number)
        } else {
            number.checked_mul(self.block_size)
        }
    }
}

/// A frame found while seeking.
#[derive(Debug, Clone, Copy)]
struct Found {
    /// Offset of the frame in the file.
    offset: u64,
    /// Position in the stream of its first sample.
    sample: u64,
    /// Samples it holds.
    len: u64,
    /// Bytes it takes in the file.
    bytes: usize,
}

/// Decodes a FLAC stream block by block, from any position.
///
/// Memory use does not depend on the length of the stream: it holds one
/// decoded frame, a window of the file about one frame long (two while a
/// seek checks that one frame is followed by the next), and the seek table.
pub(crate) struct FlacReader<R> {
    reader: R,
    info: StreamInfo,
    scale: f64,
    audio_start: u64,
    stream_len: u64,
    seek_points: Vec<SeekPoint>,
    /// `None` when the stream has no frames at all.
    numbering: Option<Numbering>,
    window: Window,
    /// How many bytes to have at hand before decoding a frame: the size of
    /// the largest frame met so far, or the encoder's word for it.
    frame_estimate: usize,
    /// The size past which something is not accepted as a frame.
    frame_limit: usize,
    decoder: FrameDecoder,
    /// Position in the stream of the first sample of the decoded frame.
    block_start: u64,
    /// Samples in the decoded frame.
    block_len: usize,
    /// How many of them have been handed out.
    block_position: usize,
    /// The length the stream declares, if it declares one.
    total: Option<u64>,
    /// Set when a failed seek left the window somewhere unknown. The next
    /// read then finds its place again.
    lost: bool,
}

impl<R: Read + Seek> FlacReader<R> {
    /// Reads the metadata of the stream in `reader` and gets ready to decode
    /// from its first frame.
    pub(crate) fn new(mut reader: R) -> Result<Self, FlacError> {
        let metadata = read_metadata(&mut reader, true)?;
        let info = metadata.info;

        // The first frame tells how frames are numbered.
        reader.seek(SeekFrom::Start(metadata.audio_start))?;
        let mut head = [0u8; MAX_FRAME_HEADER];
        let have = read_up_to(&mut reader, &mut head)?;
        let numbering = if have == 0 {
            None
        } else {
            let header = frame_header(&mut BitReader::new(&head[..have]), &info)?;
            Some(Numbering {
                variable: header.variable_blocks,
                base: header.number,
                block_size: header.block_size as u64,
            })
        };
        let window = Window::new(metadata.audio_start, metadata.stream_len);

        // A frame that stores every sample as it is, for the largest block
        // the format allows, is the largest a sensible encoder writes. Twice
        // that is accepted; beyond it, the data is not a frame.
        let subframe = 16 + (65_536 * (info.bits_per_sample as usize + 1)).div_ceil(8);
        let frame_limit = 2 * (MAX_FRAME_HEADER + 2 + info.channels * subframe);
        let frame_estimate = match info.max_frame_size {
            0 => FIRST_FRAME_GUESS,
            size => (size as usize).max(MAX_FRAME_HEADER),
        }
        .min(frame_limit);

        Ok(Self {
            reader,
            info,
            scale: info.scale(),
            audio_start: metadata.audio_start,
            stream_len: metadata.stream_len,
            seek_points: metadata.seek_points,
            numbering,
            window,
            frame_estimate,
            frame_limit,
            decoder: FrameDecoder::new(),
            block_start: 0,
            block_len: 0,
            block_position: 0,
            total: (info.total_frames > 0).then_some(info.total_frames),
            lost: false,
        })
    }

    pub(crate) fn sample_rate(&self) -> u32 {
        self.info.sample_rate
    }

    pub(crate) fn num_channels(&self) -> usize {
        self.info.channels
    }

    pub(crate) fn bits_per_sample(&self) -> u16 {
        self.info.bits_per_sample as u16
    }

    /// The length the stream declares, if it declares one.
    pub(crate) fn frames(&self) -> Option<u64> {
        self.total
    }

    pub(crate) fn position(&self) -> u64 {
        self.block_start + self.block_position as u64
    }

    pub(crate) fn into_inner(self) -> R {
        self.reader
    }

    /// Fills `out` from the current position and returns how many frames
    /// were decoded: fewer than `out` holds only at the end of the stream.
    pub(crate) fn read(&mut self, out: &mut AudioBuffer<f32>) -> Result<usize, FlacError> {
        if self.lost {
            self.reposition(self.position(), false)?;
        }
        let want = out.num_frames();
        let mut done = 0;
        while done < want {
            let left = self
                .total
                .map_or(u64::MAX, |total| total.saturating_sub(self.position()));
            if left == 0 {
                break;
            }
            if self.block_position == self.block_len {
                if !self.next_frame()? {
                    break;
                }
                continue;
            }
            let count = (self.block_len - self.block_position)
                .min(want - done)
                .min(usize::try_from(left).unwrap_or(usize::MAX));
            let from = self.block_position;
            for channel in 0..self.info.channels {
                let source = &self.decoder.block[channel][from..from + count];
                let destination = &mut out.channel_mut(channel)[done..done + count];
                for (value, &sample) in destination.iter_mut().zip(source) {
                    *value = (f64::from(sample as i32) * self.scale) as f32;
                }
            }
            self.block_position += count;
            done += count;
        }
        Ok(done)
    }

    /// Moves to `frame`, or to the end if the stream is shorter, and returns
    /// where it landed.
    ///
    /// If this fails, the position is unchanged.
    pub(crate) fn seek(&mut self, frame: u64) -> Result<u64, FlacError> {
        let target = self.total.map_or(frame, |total| frame.min(total));
        let mut nearby = false;
        if !self.lost {
            if let Some(offset) = target.checked_sub(self.block_start) {
                // Within the frame at hand, or just past it where decoding
                // would carry on anyway: nothing to look for.
                if offset <= self.block_len as u64 {
                    self.block_position = offset as usize;
                    return Ok(target);
                }
                // A short way ahead: decoding up to it costs less than
                // searching for it.
                nearby = offset - self.block_len as u64 <= self.short_walk();
            }
        }
        self.reposition(target, nearby)
    }

    /// How many samples ahead are reached sooner by decoding onwards than
    /// by searching: a few blocks.
    ///
    /// It is counted in samples and not in bytes because a frame of silence
    /// takes a dozen bytes: a stretch of the file that is read in one go
    /// can hold thousands of them.
    fn short_walk(&self) -> u64 {
        4 * u64::from(self.info.max_block_size.max(1_024))
    }

    /// Moves to `target`: by decoding onwards if it is `nearby`, by a search
    /// of the file otherwise. On failure the position is put back, and the
    /// reader is marked as lost so that the next read finds its place again.
    fn reposition(&mut self, target: u64, nearby: bool) -> Result<u64, FlacError> {
        let previous = self.position();
        let moved = if nearby {
            self.block_position = self.block_len;
            self.walk_to(target)
        } else {
            self.search(target)
        };
        match moved {
            Ok(position) => {
                self.lost = false;
                Ok(position)
            }
            Err(error) => {
                self.block_start = previous;
                self.block_len = 0;
                self.block_position = 0;
                self.lost = true;
                Err(error)
            }
        }
    }

    /// Decodes the frame that starts `skip` bytes after the window's
    /// position, having `estimate` bytes of it at hand first and more if the
    /// frame turns out longer.
    ///
    /// Returns the header and the frame's length in bytes, and leaves the
    /// window where it was. `Ok(None)` means the file ends where the frame
    /// would start.
    fn decode_at_window(
        &mut self,
        skip: usize,
        mut estimate: usize,
    ) -> Result<Option<(FrameHeader, usize)>, FlacError> {
        loop {
            self.window
                .fill(&mut self.reader, skip.saturating_add(estimate))?;
            let Some(available) = self.window.available().get(skip..) else {
                return Ok(None);
            };
            if available.is_empty() {
                return Ok(None);
            }
            let mut bits = BitReader::new(available);
            match self.decoder.decode(&mut bits, &self.info) {
                Ok(header) => return Ok(Some((header, bits.byte_position()))),
                // The frame runs past what was read, not past the file: read
                // more, up to the largest size a frame may have.
                Err(FlacError::Truncated) if !self.window.exhausted => {
                    let held = available.len();
                    if held >= self.frame_limit {
                        return Err(FlacError::Malformed(
                            "frame is larger than the format allows",
                        ));
                    }
                    estimate = (held.max(estimate) * 2).min(self.frame_limit);
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Decodes the frame after the one at hand. Returns whether there was
    /// one: `false` is the end of the stream.
    fn next_frame(&mut self) -> Result<bool, FlacError> {
        // Decoding writes over the frame at hand, whether it succeeds or
        // not, so from here on there is none: the position is the start of
        // the frame about to be decoded.
        self.block_start += self.block_len as u64;
        self.block_len = 0;
        self.block_position = 0;
        let Some((header, used)) = self.decode_at_window(0, self.frame_estimate)? else {
            return Ok(false);
        };
        self.window.position += used;
        self.frame_estimate = self.frame_estimate.max(used);
        self.block_len = header.block_size;
        Ok(true)
    }

    /// Stands before the frame that starts at `offset` in the file, with
    /// `sample` as the position of its first sample. The frame is decoded
    /// by whatever reads next.
    fn stand_before(&mut self, offset: u64, sample: u64) {
        self.window.move_to(offset);
        self.block_start = sample;
        self.block_len = 0;
        self.block_position = 0;
    }

    /// Decodes onwards from the frame at hand until `target` is in it, and
    /// stops at the end of the stream if that comes first.
    ///
    /// A damaged frame on the way is stepped over, as long as the target is
    /// in a later one: a seek fails only if the frame it is aimed at cannot
    /// be decoded, not because of damage somewhere before it.
    fn walk_to(&mut self, target: u64) -> Result<u64, FlacError> {
        loop {
            let end = self.block_start + self.block_len as u64;
            if target < end {
                self.block_position = (target - self.block_start) as usize;
                return Ok(target);
            }
            // Past the declared length nothing is decoded: what follows the
            // audio there need not be a frame.
            if self.total.is_some_and(|total| end >= total) {
                self.block_position = self.block_len;
                return Ok(end);
            }
            let damage = match self.next_frame() {
                Ok(true) => continue,
                Ok(false) => {
                    self.block_position = self.block_len;
                    return Ok(end);
                }
                Err(FlacError::Io(error)) => return Err(FlacError::Io(error)),
                Err(damage) => damage,
            };
            // The next frame that does decode says by its own number where
            // it belongs. It is of use if the target is not before it.
            let Some(numbering) = self.numbering else {
                return Err(damage);
            };
            let from = self.window.file_position() + 1;
            match self.find_frame(from, self.stream_len, numbering)? {
                Some(found) if (end..=target).contains(&found.sample) => {
                    self.stand_before(found.offset, found.sample);
                }
                _ => return Err(damage),
            }
        }
    }

    /// Finds the frame that holds `target` by narrowing down where in the
    /// file it can be, then decodes up to it.
    fn search(&mut self, target: u64) -> Result<u64, FlacError> {
        let Some(numbering) = self.numbering else {
            // A stream without frames has one position.
            return Ok(0);
        };

        // `lower` is a frame known to start at or before the target, and it
        // is where decoding will start from. `upper` is where the search
        // has no more to gain: no frame found from there on starts at or
        // before the target. Only `lower` has to be right for the seek to
        // be exact; a wrong `upper` costs time, since decoding then starts
        // further from the target than it had to.
        let mut lower = Found {
            offset: self.audio_start,
            sample: 0,
            len: numbering.block_size,
            bytes: 0,
        };
        let mut upper = self.stream_len;

        // The target is in the first frame: there is nothing to narrow down.
        let in_first_frame = target < numbering.block_size;

        // The seek table narrows the search to the stretch between two of
        // its points. It is not trusted: a point is used only if a frame
        // really starts there, and with the number the frame itself carries.
        if !in_first_frame && numbering.base == 0 {
            let after = self
                .seek_points
                .partition_point(|point| point.sample <= target);
            if after > 0 {
                let point = self.seek_points[after - 1];
                let offset = self.audio_start.saturating_add(point.offset);
                if let Some(found) = self.frame_at(offset, numbering)? {
                    if found.sample <= target {
                        lower = found;
                        if let Some(next) = self.seek_points.get(after) {
                            upper = upper.min(self.audio_start.saturating_add(next.offset));
                        }
                    }
                }
            }
        }

        // Bisection on frames. A frame's length in bytes is not in its
        // header, so the middle of the stretch is a byte offset, and the
        // frame looked at is the first one that starts after it.
        let short_walk = self.short_walk();
        loop {
            // It stops when the target is in the frame found or a short
            // walk past it, measured in samples.
            let lower_end = lower.sample + lower.len;
            if target < lower_end || target - lower_end <= short_walk {
                break;
            }
            // And when no frame can start between the two bounds. The frame
            // after `lower` starts where `lower` ends, if that is known.
            let from = lower.offset + (lower.bytes as u64).max(1);
            if upper <= from {
                break;
            }
            let middle = from + (upper - from) / 2;
            match self.find_frame(middle, upper, numbering)? {
                // Frames come in the order of their numbers, so one found
                // after `lower` starts where `lower` ends or later.
                Some(found) if (lower_end..=target).contains(&found.sample) => lower = found,
                // No frame, or one past the target: the upper half has
                // nothing to offer. A frame whose number is out of order is
                // not relied on either.
                _ => upper = middle,
            }
        }

        // Decode from the frame found to the one that holds the target.
        self.stand_before(lower.offset, lower.sample);
        self.walk_to(target)
    }

    /// Checks that a frame starts exactly at `offset` and says which.
    fn frame_at(&mut self, offset: u64, numbering: Numbering) -> Result<Option<Found>, FlacError> {
        if offset >= self.stream_len {
            return Ok(None);
        }
        self.window.move_to(offset);
        self.frame_at_window(numbering)
    }

    /// Checks that a frame of this stream starts at the window's position,
    /// and leaves the window there.
    ///
    /// The frame is decoded whole: its two checksums must match, and its
    /// number must fit the stream. That alone is not proof, because the
    /// bytes of a frame can also be audio: a stream may hold, sample by
    /// sample, something that reads as a frame. So what comes after it is
    /// checked as well. It must be a frame that decodes whole and carries
    /// the next number, or the end of the file. A real frame that is
    /// followed by neither, because the next one is damaged or because
    /// something else was appended to the file, is turned down too; it is
    /// then reached by decoding onwards from a frame before it.
    fn frame_at_window(&mut self, numbering: Numbering) -> Result<Option<Found>, FlacError> {
        let offset = self.window.file_position();
        let (header, used) = match self.decode_at_window(0, self.frame_estimate) {
            Ok(Some(decoded)) => decoded,
            Ok(None) => return Ok(None),
            Err(FlacError::Io(error)) => return Err(FlacError::Io(error)),
            // Whatever starts here is not a frame.
            Err(_) => return Ok(None),
        };
        let Some(sample) = numbering.sample_of(&header) else {
            return Ok(None);
        };
        if self.total.is_some_and(|total| sample >= total) {
            return Ok(None);
        }
        let len = header.block_size as u64;

        let followed = match self.decode_at_window(used, self.frame_estimate) {
            Ok(Some((next, _))) => numbering.sample_of(&next) == Some(sample + len),
            // The file ends with this frame.
            Ok(None) => true,
            Err(FlacError::Io(error)) => return Err(FlacError::Io(error)),
            Err(_) => false,
        };
        if !followed {
            return Ok(None);
        }
        self.frame_estimate = self.frame_estimate.max(used);
        Ok(Some(Found {
            offset,
            sample,
            len,
            bytes: used,
        }))
    }

    /// Finds the first frame that starts in `from..limit`, and leaves the
    /// window at it.
    fn find_frame(
        &mut self,
        from: u64,
        limit: u64,
        numbering: Numbering,
    ) -> Result<Option<Found>, FlacError> {
        // Every frame of a stream starts with the same two bytes: the sync
        // code of thirteen ones and a zero, a reserved zero, and the bit
        // that says how frames are numbered.
        let sync = [0xFF, if numbering.variable { 0xF9 } else { 0xF8 }];
        self.window.move_to(from);
        loop {
            self.window.fill(&mut self.reader, self.frame_estimate)?;
            let offset = self.window.file_position();
            let available = self.window.available();
            if offset >= limit || available.len() < 2 {
                return Ok(None);
            }
            let span = usize::try_from(limit - offset)
                .unwrap_or(usize::MAX)
                .min(available.len() - 1);
            let candidate = available
                .windows(2)
                .take(span)
                .position(|pair| pair == sync);
            match candidate {
                None => self.window.position += span,
                Some(index) => {
                    self.window.position += index;
                    if let Some(found) = self.frame_at_window(numbering)? {
                        return Ok(Some(found));
                    }
                    // Those two bytes were part of the audio: look further.
                    self.window.position += 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_reader_reads_across_bytes() {
        let mut reader = BitReader::new(&[0b1010_0000, 0b0001_1111, 0xFF]);
        assert_eq!(reader.bits(3).unwrap(), 0b101);
        assert_eq!(reader.unary().unwrap(), 8);
        assert!(!reader.is_aligned());
        assert_eq!(reader.signed(4).unwrap(), -1);
        assert!(reader.is_aligned());
        assert_eq!(reader.signed(8).unwrap(), -1);
        assert!(reader.at_end());
        assert!(matches!(reader.bits(1), Err(FlacError::Truncated)));
        assert!(matches!(reader.unary(), Err(FlacError::Truncated)));
    }

    #[test]
    fn checksums_match_known_values() {
        // The standard check string for both polynomials.
        assert_eq!(crc8(b"123456789"), 0xF4);
        assert_eq!(crc16(b"123456789"), 0xFEE8);
    }

    #[test]
    fn fixed_predictors_reproduce_polynomials() {
        // A cubic is predicted exactly by the order-4 predictor, so its
        // residuals after the warm-up are zero.
        let cubic: Vec<i64> = (0..12).map(|n: i64| n * n * n - 5 * n).collect();
        let mut samples = cubic[..4].to_vec();
        samples.resize(12, 0);
        predict(&mut samples, &[4, -6, 4, -1], 0);
        assert_eq!(samples, cubic);
    }

    #[test]
    fn rejects_what_is_not_flac() {
        assert!(matches!(decode(b""), Err(FlacError::NotFlac)));
        assert!(matches!(decode(b"RIFF1234WAVE"), Err(FlacError::NotFlac)));
        assert!(matches!(decode(b"fLaC"), Err(FlacError::Truncated)));
        // A last metadata block that is not stream information.
        assert!(matches!(
            decode(b"fLaC\x81\x00\x00\x00"),
            Err(FlacError::Malformed(_))
        ));
    }

    /// A frame header for a mono 16-bit stream, with its checksum.
    fn header_bytes(variable: bool, number: &[u8]) -> Vec<u8> {
        // Block size code 12 is 4096 samples; rate and sample size are
        // taken from the stream information.
        let mut bytes = vec![0xFF, 0xF8 | u8::from(variable), 0xC0, 0x00];
        bytes.extend_from_slice(number);
        bytes.push(crc8(&bytes));
        bytes
    }

    fn mono_info() -> StreamInfo {
        StreamInfo {
            max_block_size: 4_096,
            max_frame_size: 0,
            sample_rate: 44_100,
            channels: 1,
            bits_per_sample: 16,
            total_frames: 0,
        }
    }

    #[test]
    fn frame_numbers_are_decoded_at_every_length() {
        // The smallest and the largest number of each length of the coding,
        // which is that of UTF-8 carried on to seven bytes.
        for (bytes, number) in [
            (&[0x00][..], 0u64),
            (&[0x7F], 0x7F),
            (&[0xC2, 0x80], 0x80),
            (&[0xDF, 0xBF], 0x7FF),
            (&[0xE0, 0xA0, 0x80], 0x800),
            (&[0xEF, 0xBF, 0xBF], 0xFFFF),
            (&[0xF0, 0x90, 0x80, 0x80], 0x1_0000),
            (&[0xF7, 0xBF, 0xBF, 0xBF], 0x1F_FFFF),
            (&[0xFB, 0xBF, 0xBF, 0xBF, 0xBF], 0x3FF_FFFF),
            (&[0xFD, 0xBF, 0xBF, 0xBF, 0xBF, 0xBF], 0x7FFF_FFFF),
        ] {
            let header = header_bytes(false, bytes);
            let parsed = frame_header(&mut BitReader::new(&header), &mono_info()).unwrap();
            assert_eq!(parsed.number, number, "{bytes:02X?}");
            assert_eq!(parsed.block_size, 4_096);
            assert!(!parsed.variable_blocks);
        }

        // Seven bytes carry the 36 bits of a sample number, and only that.
        let longest = [0xFE, 0xBF, 0xBF, 0xBF, 0xBF, 0xBF, 0xBF];
        let header = header_bytes(true, &longest);
        let parsed = frame_header(&mut BitReader::new(&header), &mono_info()).unwrap();
        assert_eq!(parsed.number, 0xF_FFFF_FFFF);
        assert!(parsed.variable_blocks);
        assert!(header.len() <= MAX_FRAME_HEADER);
        let header = header_bytes(false, &longest);
        assert!(frame_header(&mut BitReader::new(&header), &mono_info()).is_err());
    }

    /// A frame number in the coding of frame headers, up to 21 bits.
    fn coded_number(number: u64) -> Vec<u8> {
        let six = |shift: u32| 0x80 | ((number >> shift) & 0x3F) as u8;
        match number {
            0..=0x7F => vec![number as u8],
            0x80..=0x7FF => vec![0xC0 | (number >> 6) as u8, six(0)],
            0x800..=0xFFFF => vec![0xE0 | (number >> 12) as u8, six(6), six(0)],
            _ => vec![0xF0 | (number >> 18) as u8, six(12), six(6), six(0)],
        }
    }

    /// The value every sample of frame `index` of [`constant_stream`] has.
    fn constant_value(index: u64) -> i16 {
        (index % 30_000) as i16
    }

    /// A mono 16-bit stream of `frames` frames of 4096 samples, each frame
    /// holding one value. This is how silence is stored: 13 to 15 bytes a
    /// frame, whatever the number of samples.
    fn constant_stream(frames: u64) -> Vec<u8> {
        let mut bytes = b"fLaC\x80\x00\x00\x22".to_vec();
        // Blocks of 4096 samples, frame sizes not stated.
        bytes.extend_from_slice(&[0x10, 0x00, 0x10, 0x00, 0, 0, 0, 0, 0, 0]);
        // 48 kHz in 20 bits, one channel in 3, 16 bits in 5, and the length
        // in 36.
        let packed = (48_000u64 << 44) | (15 << 36) | (frames * 4_096);
        bytes.extend_from_slice(&packed.to_be_bytes());
        bytes.extend_from_slice(&[0; 16]);
        for index in 0..frames {
            let mut frame = header_bytes(false, &coded_number(index));
            // A subframe of type 0, one value, with no wasted bits.
            frame.push(0x00);
            frame.extend_from_slice(&constant_value(index).to_be_bytes());
            let frame_crc = crc16(&frame);
            frame.extend_from_slice(&frame_crc.to_be_bytes());
            bytes.extend_from_slice(&frame);
        }
        bytes
    }

    #[test]
    fn a_seek_through_silence_decodes_a_few_frames_and_not_thousands() {
        // A minute and a half in 14 KB, less than is read from a file in
        // one go, and half an hour in 290 KB. Measured in bytes, any two
        // places in the first are close and a seek could decode its way
        // from one to the other; measured in samples they are not.
        for frames in [1_000u64, 20_000] {
            let bytes = constant_stream(frames);
            let total = frames * 4_096;
            let mut reader = FlacReader::new(Cursor::new(bytes.as_slice())).unwrap();
            assert_eq!(reader.frames(), Some(total));
            let mut out = AudioBuffer::<f32>::new(1, 16);

            // Reading from the start, for what the seeks are held against.
            for index in 0..frames.min(50) * 256 {
                assert_eq!(reader.read(&mut out).unwrap(), 16);
                let frame = index * 16 / 4_096;
                let expected = f32::from(constant_value(frame)) / 32_768.0;
                assert!(out.channel(0).iter().all(|&value| value == expected));
            }

            let mut state = 0x2545_F491_4F6C_DD1D_u64 ^ frames;
            let mut worst = 0;
            for _ in 0..500 {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let target = state % (total - 16);
                let before = reader.decoder.attempts;
                assert_eq!(reader.seek(target).unwrap(), target);
                assert_eq!(reader.read(&mut out).unwrap(), 16);
                worst = worst.max(reader.decoder.attempts - before);
                for (offset, &value) in out.channel(0).iter().enumerate() {
                    let frame = (target + offset as u64) / 4_096;
                    assert_eq!(value, f32::from(constant_value(frame)) / 32_768.0);
                }
            }
            println!("{frames} frames of silence: a seek decoded at most {worst} frames");
            // Two frames for every halving of the file, the frame found and
            // the one after it, and a few more to walk to the target. Before
            // seeks were measured in samples this was up to 1000 for the
            // short stream, decoded from its first frame, and more than
            // 2000 for the long one.
            assert!(worst <= 48, "a seek decoded {worst} frames");
        }
    }
}
