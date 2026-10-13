//! FLAC, the Free Lossless Audio Codec.
//!
//! A decoder written from the format's specification, RFC 9639. It reads
//! every kind of frame the format defines: constant, verbatim, fixed and
//! linear-prediction subframes; both Rice codings with escapes; wasted
//! bits; all three stereo decorrelations; 4 to 32 bits per sample; up to
//! eight channels. The checksums on every frame are verified.
//!
//! ```no_run
//! let file = manguebyte_formats::flac::read_file("song.flac")?;
//! println!("{} Hz, {} channels, {} frames",
//!     file.sample_rate, file.buffer.num_channels(), file.buffer.num_frames());
//! # Ok::<(), manguebyte_formats::flac::FlacError>(())
//! ```
//!
//! Encoding is not implemented yet.

use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::Path;

use manguebyte_core::AudioBuffer;

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

/// What went wrong while reading a FLAC file.
#[derive(Debug)]
pub enum FlacError {
    /// The underlying reader failed.
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
    fn unary(&mut self) -> Result<u32, FlacError> {
        let mut zeros = 0u32;
        loop {
            if self.position >= self.bytes.len() * 8 {
                return Err(FlacError::Truncated);
            }
            // Whole zero bytes can be skipped at once.
            let byte = self.bytes[self.position / 8];
            let offset = (self.position % 8) as u32;
            let rest = byte << offset;
            if rest == 0 {
                zeros += 8 - offset;
                self.position += (8 - offset) as usize;
                continue;
            }
            let leading = rest.leading_zeros();
            zeros += leading;
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
struct StreamInfo {
    sample_rate: u32,
    channels: usize,
    bits_per_sample: u32,
    total_frames: u64,
}

fn decode(bytes: &[u8]) -> Result<FlacFile, FlacError> {
    if bytes.len() < 4 || &bytes[..4] != b"fLaC" {
        return Err(FlacError::NotFlac);
    }
    let mut position = 4;
    let mut info = None;
    loop {
        let header = bytes
            .get(position..position + 4)
            .ok_or(FlacError::Truncated)?;
        let is_last = header[0] & 0x80 != 0;
        let kind = header[0] & 0x7F;
        let length = u32::from_be_bytes([0, header[1], header[2], header[3]]) as usize;
        let body = bytes
            .get(position + 4..position + 4 + length)
            .ok_or(FlacError::Truncated)?;
        if kind == 0 {
            info = Some(stream_info(body)?);
        }
        position += 4 + length;
        if is_last {
            break;
        }
    }
    let info = info.ok_or(FlacError::Malformed("no stream information block"))?;

    // The length is known up front in almost every file. It is trusted only
    // up to what the data could possibly hold, so that a damaged header
    // cannot ask for an enormous allocation.
    let plausible = (bytes.len() as u64).saturating_mul(64);
    let expected = info.total_frames.min(plausible) as usize;
    let mut channels: Vec<Vec<i32>> = vec![Vec::with_capacity(expected); info.channels];

    let mut reader = BitReader::new(&bytes[position..]);
    while !reader.at_end() {
        decode_frame(&mut reader, &info, &mut channels)?;
    }

    let frames = channels.first().map_or(0, Vec::len);
    let scale = 1.0 / (1u64 << (info.bits_per_sample - 1)) as f64;
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

fn stream_info(body: &[u8]) -> Result<StreamInfo, FlacError> {
    if body.len() < 34 {
        return Err(FlacError::Malformed("stream information block too short"));
    }
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
        sample_rate,
        channels,
        bits_per_sample,
        total_frames,
    })
}

/// How the channels of a frame were combined before coding.
#[derive(Clone, Copy, PartialEq)]
enum Stereo {
    Independent,
    LeftSide,
    SideRight,
    MidSide,
}

fn decode_frame(
    reader: &mut BitReader<'_>,
    info: &StreamInfo,
    output: &mut [Vec<i32>],
) -> Result<(), FlacError> {
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

    // The frame or sample number, coded like UTF-8 but up to 36 bits. Its
    // value is not needed to decode in order, only its length.
    let first = reader.bits(8)? as u8;
    let extra = match first.leading_ones() {
        0 => 0,
        ones @ 2..=7 => ones - 1,
        _ => return Err(FlacError::Malformed("invalid frame number")),
    };
    if !variable_blocks && extra > 5 {
        return Err(FlacError::Malformed("invalid frame number"));
    }
    for _ in 0..extra {
        if reader.bits(8)? & 0xC0 != 0x80 {
            return Err(FlacError::Malformed("invalid frame number"));
        }
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
    let (channel_count, stereo) = match channel_code {
        code @ 0..=7 => (code as usize + 1, Stereo::Independent),
        8 => (2, Stereo::LeftSide),
        9 => (2, Stereo::SideRight),
        10 => (2, Stereo::MidSide),
        _ => return Err(FlacError::Malformed("reserved channel assignment")),
    };
    if channel_count != output.len() || bits_per_sample != info.bits_per_sample {
        return Err(FlacError::Malformed(
            "frame disagrees with the stream information",
        ));
    }

    let header_end = reader.byte_position();
    let header_crc = reader.bits(8)? as u8;
    if crc8(&reader.bytes[frame_start..header_end]) != header_crc {
        return Err(FlacError::Corrupt);
    }

    let mut block: [Vec<i64>; 8] = Default::default();
    for (index, samples) in block.iter_mut().take(channel_count).enumerate() {
        // The side channel is a difference, so it needs one more bit.
        let is_side = matches!(
            (stereo, index),
            (Stereo::LeftSide, 1) | (Stereo::SideRight, 0) | (Stereo::MidSide, 1)
        );
        *samples = decode_subframe(reader, block_size, bits_per_sample + u32::from(is_side))?;
    }

    reader.align();
    let frame_end = reader.byte_position();
    let frame_crc = reader.bits(16)? as u16;
    if crc16(&reader.bytes[frame_start..frame_end]) != frame_crc {
        return Err(FlacError::Corrupt);
    }

    let (first, rest) = block.split_at_mut(1);
    let (a, b) = (&mut first[0], &mut rest[0]);
    match stereo {
        Stereo::Independent => {}
        Stereo::LeftSide => {
            for (left, side) in a.iter().zip(b.iter_mut()) {
                *side = left - *side;
            }
        }
        Stereo::SideRight => {
            for (side, right) in a.iter_mut().zip(b.iter()) {
                *side += right;
            }
        }
        Stereo::MidSide => {
            for (mid, side) in a.iter_mut().zip(b.iter_mut()) {
                // The mid channel lost its lowest bit when it was halved;
                // it is the same as the side channel's.
                let sum = (*mid << 1) | (*side & 1);
                let difference = *side;
                *mid = (sum + difference) >> 1;
                *side = (sum - difference) >> 1;
            }
        }
    }

    for (out, samples) in output.iter_mut().zip(&block) {
        out.extend(samples.iter().map(|&sample| sample as i32));
    }
    Ok(())
}

fn decode_subframe(
    reader: &mut BitReader<'_>,
    block_size: usize,
    bits_per_sample: u32,
) -> Result<Vec<i64>, FlacError> {
    if reader.bits(1)? != 0 {
        return Err(FlacError::Malformed("padding bit set in subframe header"));
    }
    let kind = reader.bits(6)? as u32;
    let wasted = if reader.bits(1)? == 1 {
        reader.unary()? + 1
    } else {
        0
    };
    if wasted >= bits_per_sample {
        return Err(FlacError::Malformed("more wasted bits than bits"));
    }
    let bits = bits_per_sample - wasted;

    let mut samples = Vec::with_capacity(block_size);
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
            decode_residual(reader, block_size, order, &mut samples)?;
            const FIXED: [&[i64]; 5] = [&[], &[1], &[2, -1], &[3, -3, 1], &[4, -6, 4, -1]];
            predict(&mut samples, FIXED[order], 0);
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
            decode_residual(reader, block_size, order, &mut samples)?;
            predict(&mut samples, &coefficients[..order], shift as u32);
        }
        _ => return Err(FlacError::Malformed("reserved subframe type")),
    }

    if wasted > 0 {
        for sample in &mut samples {
            *sample <<= wasted;
        }
    }
    Ok(samples)
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
                let quotient = u64::from(reader.unary()?);
                let folded = (quotient << parameter) | reader.bits(parameter)?;
                // Even numbers are positive, odd ones negative.
                samples.push((folded >> 1) as i64 ^ -((folded & 1) as i64));
            }
        }
    }
    Ok(())
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
}
