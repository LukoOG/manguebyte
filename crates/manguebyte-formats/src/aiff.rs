//! AIFF and AIFF-C files.
//!
//! Reads integer PCM from 1 to 32 bits, big-endian as the format defines it
//! or little-endian (`sowt`), and 32 and 64-bit float. Writes 16 and 24-bit
//! PCM and 32-bit float.
//!
//! ```
//! use manguebyte_core::AudioBuffer;
//! use manguebyte_formats::aiff::{self, AiffFormat};
//!
//! let mut buffer = AudioBuffer::<f32>::new(2, 3);
//! buffer.channel_mut(0).copy_from_slice(&[0.0, 0.5, -0.5]);
//!
//! let mut bytes = Vec::new();
//! aiff::write(&mut bytes, &buffer, 44_100, AiffFormat::Int16).unwrap();
//!
//! let file = aiff::read(bytes.as_slice()).unwrap();
//! assert_eq!(file.sample_rate, 44_100);
//! assert_eq!(file.buffer, buffer);
//! ```

use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

use manguebyte_core::AudioBuffer;

/// How samples are stored in an AIFF file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AiffFormat {
    /// 16-bit signed integer PCM.
    Int16,
    /// 24-bit signed integer PCM.
    Int24,
    /// 32-bit IEEE float, written as AIFF-C.
    Float32,
}

/// The contents of an AIFF file.
#[derive(Debug, Clone, PartialEq)]
pub struct AiffFile {
    /// The audio, converted to `f32` with integer full scale mapped to
    /// `±1.0`.
    pub buffer: AudioBuffer<f32>,
    /// Sample rate in hertz, rounded to a whole number.
    pub sample_rate: u32,
    /// Bit depth the samples were stored with.
    pub bits_per_sample: u16,
    /// Whether the samples were stored as floating point.
    pub is_float: bool,
}

/// What went wrong while reading or writing an AIFF file.
#[derive(Debug)]
pub enum AiffError {
    /// The underlying reader or writer failed.
    Io(io::Error),
    /// The data is not an AIFF or AIFF-C file.
    NotAiff,
    /// The file is damaged or incomplete.
    Malformed(&'static str),
    /// The file is valid but uses an encoding this crate does not handle.
    Unsupported(&'static str),
    /// The audio is too long for the 4 GiB limit of the format.
    TooLarge,
}

impl fmt::Display for AiffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::NotAiff => f.write_str("not an AIFF file"),
            Self::Malformed(what) => write!(f, "malformed AIFF file: {what}"),
            Self::Unsupported(what) => write!(f, "unsupported AIFF file: {what}"),
            Self::TooLarge => f.write_str("audio exceeds the 4 GiB AIFF size limit"),
        }
    }
}

impl std::error::Error for AiffError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for AiffError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Reads an AIFF file from `reader`. The whole stream is loaded into memory.
pub fn read<R: Read>(mut reader: R) -> Result<AiffFile, AiffError> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    parse(&bytes)
}

/// Reads the AIFF file at `path`.
pub fn read_file<P: AsRef<Path>>(path: P) -> Result<AiffFile, AiffError> {
    read(BufReader::new(File::open(path)?))
}

/// How the sample bytes are to be understood.
#[derive(Clone, Copy, PartialEq)]
enum Encoding {
    BigEndianInt,
    LittleEndianInt,
    Float32,
    Float64,
}

/// Reads the 80-bit extended float AIFF stores its sample rate in.
fn read_extended(bytes: &[u8]) -> f64 {
    let exponent = i32::from(u16::from_be_bytes([bytes[0], bytes[1]]) & 0x7FFF);
    let mantissa = u64::from_be_bytes([
        bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8], bytes[9],
    ]);
    // The mantissa has an explicit leading one, 63 places up.
    mantissa as f64 * 2f64.powi(exponent - 16_383 - 63)
}

/// Writes a positive whole number as an 80-bit extended float.
fn write_extended(value: u32) -> [u8; 10] {
    let mut bytes = [0u8; 10];
    if value == 0 {
        return bytes;
    }
    // Normalise so that the top bit of the mantissa is set.
    let shift = u64::from(value).leading_zeros();
    let mantissa = u64::from(value) << shift;
    let exponent = (16_383 + 63 - shift) as u16;
    bytes[..2].copy_from_slice(&exponent.to_be_bytes());
    bytes[2..].copy_from_slice(&mantissa.to_be_bytes());
    bytes
}

fn parse(bytes: &[u8]) -> Result<AiffFile, AiffError> {
    if bytes.len() < 12 || &bytes[..4] != b"FORM" {
        return Err(AiffError::NotAiff);
    }
    let is_compressed_form = match &bytes[8..12] {
        b"AIFF" => false,
        b"AIFC" => true,
        _ => return Err(AiffError::NotAiff),
    };

    let mut common: Option<(usize, usize, u32, f64, Encoding)> = None;
    let mut sound: Option<&[u8]> = None;
    let mut position = 12;
    while position + 8 <= bytes.len() {
        let id = &bytes[position..position + 4];
        let size = u32::from_be_bytes([
            bytes[position + 4],
            bytes[position + 5],
            bytes[position + 6],
            bytes[position + 7],
        ]) as usize;
        let start = position + 8;
        // A sound chunk that claims to run past the end is read as far as
        // the data goes: some writers leave the size unfinished.
        let end = start.saturating_add(size).min(bytes.len());
        let body = &bytes[start..end];
        match id {
            b"COMM" => {
                if body.len() < 18 {
                    return Err(AiffError::Malformed("common chunk too short"));
                }
                let channels = u16::from_be_bytes([body[0], body[1]]) as usize;
                let frames = u32::from_be_bytes([body[2], body[3], body[4], body[5]]) as usize;
                let bits = u32::from(u16::from_be_bytes([body[6], body[7]]));
                let rate = read_extended(&body[8..18]);
                let encoding = if is_compressed_form {
                    match body.get(18..22) {
                        Some(b"NONE") | Some(b"twos") => Encoding::BigEndianInt,
                        Some(b"sowt") => Encoding::LittleEndianInt,
                        Some(b"fl32") | Some(b"FL32") => Encoding::Float32,
                        Some(b"fl64") | Some(b"FL64") => Encoding::Float64,
                        Some(_) => return Err(AiffError::Unsupported("compressed audio")),
                        None => return Err(AiffError::Malformed("common chunk too short")),
                    }
                } else {
                    Encoding::BigEndianInt
                };
                common = Some((channels, frames, bits, rate, encoding));
            }
            b"SSND" => {
                if body.len() < 8 {
                    return Err(AiffError::Malformed("sound chunk too short"));
                }
                let offset = u32::from_be_bytes([body[0], body[1], body[2], body[3]]) as usize;
                sound = Some(body.get(8 + offset..).unwrap_or(&[]));
            }
            _ => {}
        }
        // Chunks are padded to an even length.
        position = start.saturating_add(size).saturating_add(size % 2);
    }

    let (channels, frames, bits, rate, encoding) =
        common.ok_or(AiffError::Malformed("no common chunk"))?;
    if channels == 0 {
        return Err(AiffError::Malformed("no channels"));
    }
    if !(1.0..=4_294_967_295.0).contains(&rate) {
        return Err(AiffError::Malformed("invalid sample rate"));
    }
    let bytes_per_sample = match encoding {
        Encoding::Float32 => 4,
        Encoding::Float64 => 8,
        _ => {
            if !(1..=32).contains(&bits) {
                return Err(AiffError::Unsupported("bit depth above 32"));
            }
            bits.div_ceil(8) as usize
        }
    };
    let data = sound.unwrap_or(&[]);
    // Trust the data over the header if the two disagree.
    let frames = frames.min(data.len() / (bytes_per_sample * channels));

    let mut buffer = AudioBuffer::new(channels, frames);
    for (index, sample) in data
        .chunks_exact(bytes_per_sample)
        .take(frames * channels)
        .enumerate()
    {
        let value = match encoding {
            Encoding::Float32 => f32::from_be_bytes([sample[0], sample[1], sample[2], sample[3]]),
            Encoding::Float64 => f64::from_be_bytes([
                sample[0], sample[1], sample[2], sample[3], sample[4], sample[5], sample[6],
                sample[7],
            ]) as f32,
            Encoding::BigEndianInt | Encoding::LittleEndianInt => {
                // Samples are left-justified in their bytes, so placing them
                // at the top of 32 bits scales every depth the same way.
                let mut raw = [0u8; 4];
                if encoding == Encoding::BigEndianInt {
                    raw[..bytes_per_sample].copy_from_slice(sample);
                } else {
                    for (slot, &byte) in raw.iter_mut().zip(sample.iter().rev()) {
                        *slot = byte;
                    }
                }
                (f64::from(i32::from_be_bytes(raw)) / 2_147_483_648.0) as f32
            }
        };
        buffer.channel_mut(index % channels)[index / channels] = value;
    }

    Ok(AiffFile {
        buffer,
        sample_rate: rate.round() as u32,
        bits_per_sample: match encoding {
            Encoding::Float32 => 32,
            Encoding::Float64 => 64,
            _ => bits as u16,
        },
        is_float: matches!(encoding, Encoding::Float32 | Encoding::Float64),
    })
}

/// Writes `buffer` to `writer` as an AIFF file.
///
/// Integer formats clip to full scale.
pub fn write<W: Write>(
    mut writer: W,
    buffer: &AudioBuffer<f32>,
    sample_rate: u32,
    format: AiffFormat,
) -> Result<(), AiffError> {
    let channels = u16::try_from(buffer.num_channels()).map_err(|_| AiffError::TooLarge)?;
    let frames = u32::try_from(buffer.num_frames()).map_err(|_| AiffError::TooLarge)?;
    let (bytes_per_sample, bits, is_float): (usize, u16, bool) = match format {
        AiffFormat::Int16 => (2, 16, false),
        AiffFormat::Int24 => (3, 24, false),
        AiffFormat::Float32 => (4, 32, true),
    };
    let data_len = buffer
        .num_frames()
        .checked_mul(buffer.num_channels() * bytes_per_sample)
        .filter(|len| *len < u32::MAX as usize - 64)
        .ok_or(AiffError::TooLarge)?;

    // AIFF-C adds a compression type and its name to the common chunk.
    let name = b"\x00\x00";
    let common_len: u32 = if is_float { 18 + 4 + 2 } else { 18 };
    let padding = data_len % 2;
    let form_len = 4 + (8 + common_len) + (8 + 8 + data_len as u32) + padding as u32;

    writer.write_all(b"FORM")?;
    writer.write_all(&form_len.to_be_bytes())?;
    writer.write_all(if is_float { b"AIFC" } else { b"AIFF" })?;

    writer.write_all(b"COMM")?;
    writer.write_all(&common_len.to_be_bytes())?;
    writer.write_all(&channels.to_be_bytes())?;
    writer.write_all(&frames.to_be_bytes())?;
    writer.write_all(&bits.to_be_bytes())?;
    writer.write_all(&write_extended(sample_rate))?;
    if is_float {
        writer.write_all(b"fl32")?;
        writer.write_all(name)?;
    }

    writer.write_all(b"SSND")?;
    writer.write_all(&(8 + data_len as u32).to_be_bytes())?;
    writer.write_all(&[0; 8])?;
    for frame in 0..buffer.num_frames() {
        for channel in 0..buffer.num_channels() {
            let sample = buffer.channel(channel)[frame];
            match format {
                AiffFormat::Int16 => {
                    let value = (f64::from(sample) * 32_768.0)
                        .round()
                        .clamp(-32_768.0, 32_767.0);
                    writer.write_all(&(value as i16).to_be_bytes())?;
                }
                AiffFormat::Int24 => {
                    let value = (f64::from(sample) * 8_388_608.0)
                        .round()
                        .clamp(-8_388_608.0, 8_388_607.0);
                    writer.write_all(&(value as i32).to_be_bytes()[1..])?;
                }
                AiffFormat::Float32 => writer.write_all(&sample.to_be_bytes())?,
            }
        }
    }
    if padding == 1 {
        writer.write_all(&[0])?;
    }
    writer.flush()?;
    Ok(())
}

/// Writes `buffer` to the file at `path` as an AIFF file.
pub fn write_file<P: AsRef<Path>>(
    path: P,
    buffer: &AudioBuffer<f32>,
    sample_rate: u32,
    format: AiffFormat,
) -> Result<(), AiffError> {
    write(
        BufWriter::new(File::create(path)?),
        buffer,
        sample_rate,
        format,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(channels: usize, frames: usize) -> AudioBuffer<f32> {
        let mut buffer = AudioBuffer::new(channels, frames);
        for (index, channel) in buffer.iter_channels_mut().enumerate() {
            for (frame, sample) in channel.iter_mut().enumerate() {
                // Values that 16 bits can hold exactly.
                *sample = ((frame * 37 + index * 11) % 200) as f32 / 256.0 - 0.25;
            }
        }
        buffer
    }

    #[test]
    fn round_trips_every_format_and_odd_lengths() {
        for format in [AiffFormat::Int16, AiffFormat::Int24, AiffFormat::Float32] {
            for (channels, frames) in [(1, 7), (2, 64), (3, 5), (1, 0)] {
                let buffer = ramp(channels, frames);
                let mut bytes = Vec::new();
                write(&mut bytes, &buffer, 48_000, format).unwrap();
                assert_eq!(bytes.len() % 2, 0, "{format:?}: chunks must be padded");
                assert_eq!(
                    u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize,
                    bytes.len() - 8
                );

                let file = read(bytes.as_slice()).unwrap();
                assert_eq!(file.buffer, buffer, "{format:?} {channels}x{frames}");
                assert_eq!(file.sample_rate, 48_000);
                assert_eq!(file.is_float, format == AiffFormat::Float32);
            }
        }
    }

    #[test]
    fn sample_rates_survive_the_extended_float() {
        for rate in [
            1u32, 8_000, 11_025, 44_100, 48_000, 96_000, 192_000, 2_822_400,
        ] {
            assert_eq!(read_extended(&write_extended(rate)), f64::from(rate));
        }
        // The value every AIFF tool writes for 44.1 kHz.
        assert_eq!(
            write_extended(44_100),
            [0x40, 0x0E, 0xAC, 0x44, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
        );
    }

    #[test]
    fn integer_formats_clip_instead_of_wrapping() {
        let mut buffer = AudioBuffer::<f32>::new(1, 2);
        buffer.channel_mut(0).copy_from_slice(&[2.0, -2.0]);
        let mut bytes = Vec::new();
        write(&mut bytes, &buffer, 44_100, AiffFormat::Int16).unwrap();
        let file = read(bytes.as_slice()).unwrap();
        assert!(file.buffer.channel(0)[0] > 0.999);
        assert_eq!(file.buffer.channel(0)[1], -1.0);
    }

    #[test]
    fn reads_little_endian_aifc() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"FORM\0\0\0\0AIFC");
        bytes.extend_from_slice(b"COMM\0\0\0\x18");
        bytes.extend_from_slice(&[0, 1, 0, 0, 0, 2, 0, 16]);
        bytes.extend_from_slice(&write_extended(22_050));
        bytes.extend_from_slice(b"sowt\0\0");
        bytes.extend_from_slice(b"SSND\0\0\0\x0C\0\0\0\0\0\0\0\0");
        bytes.extend_from_slice(&[0x00, 0x40, 0x00, 0xC0]); // 0x4000, 0xC000
        let file = read(bytes.as_slice()).unwrap();
        assert_eq!(file.sample_rate, 22_050);
        assert_eq!(file.buffer.channel(0), &[0.5, -0.5]);
    }

    #[test]
    fn bad_input_is_an_error_and_never_a_panic() {
        assert!(matches!(read(&b""[..]), Err(AiffError::NotAiff)));
        assert!(matches!(
            read(&b"FORM\0\0\0\0WAVE"[..]),
            Err(AiffError::NotAiff)
        ));
        assert!(matches!(
            read(&b"FORM\0\0\0\0AIFF"[..]),
            Err(AiffError::Malformed(_))
        ));

        let mut valid = Vec::new();
        write(&mut valid, &ramp(2, 16), 44_100, AiffFormat::Int24).unwrap();
        for len in 0..valid.len() {
            let _ = read(&valid[..len]);
        }
        for index in 0..valid.len() {
            for value in [0x00, 0x7F, 0x80, 0xFF] {
                let mut damaged = valid.clone();
                damaged[index] = value;
                let _ = read(damaged.as_slice());
            }
        }
    }
}
