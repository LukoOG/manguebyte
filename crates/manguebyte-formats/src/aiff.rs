//! AIFF and AIFF-C files.
//!
//! Reads integer PCM from 1 to 32 bits, big-endian as the format defines it
//! or little-endian (`sowt`), and 32 and 64-bit float. Writes 16 and 24-bit
//! PCM and 32-bit float.
//!
//! The functions here hold a whole file in memory. To read or write block by
//! block, use [`AudioReader`](crate::AudioReader) and
//! [`AudioWriter`](crate::AudioWriter).
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
use std::io::{self, BufReader, BufWriter, Cursor, Read, Seek, SeekFrom, Write};
use std::path::Path;

use manguebyte_core::AudioBuffer;

use crate::pcm::{self, PcmLayout, PcmReader, SampleDecoder, SampleEncoder};

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

impl AiffFormat {
    pub(crate) fn bytes_per_sample(self) -> usize {
        match self {
            Self::Int16 => 2,
            Self::Int24 => 3,
            Self::Float32 => 4,
        }
    }

    fn is_float(self) -> bool {
        self == Self::Float32
    }
}

impl SampleEncoder for AiffFormat {
    fn encode(self, sample: f32, bytes: &mut [u8]) {
        match self {
            Self::Int16 => {
                let value = (f64::from(sample) * 32_768.0)
                    .round()
                    .clamp(-32_768.0, 32_767.0);
                bytes.copy_from_slice(&(value as i16).to_be_bytes());
            }
            Self::Int24 => {
                let value = (f64::from(sample) * 8_388_608.0)
                    .round()
                    .clamp(-8_388_608.0, 8_388_607.0);
                bytes.copy_from_slice(&(value as i32).to_be_bytes()[1..]);
            }
            Self::Float32 => bytes.copy_from_slice(&sample.to_be_bytes()),
        }
    }
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
#[derive(Debug, Clone, Copy, PartialEq)]
enum Encoding {
    BigEndianInt,
    LittleEndianInt,
    Float32,
    Float64,
}

/// Decodes the samples of an AIFF stream.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AiffDecoder {
    encoding: Encoding,
}

impl SampleDecoder for AiffDecoder {
    fn decode(self, sample: &[u8]) -> f32 {
        match self.encoding {
            Encoding::Float32 => f32::from_be_bytes([sample[0], sample[1], sample[2], sample[3]]),
            Encoding::Float64 => f64::from_be_bytes([
                sample[0], sample[1], sample[2], sample[3], sample[4], sample[5], sample[6],
                sample[7],
            ]) as f32,
            Encoding::BigEndianInt | Encoding::LittleEndianInt => {
                // Samples are left-justified in their bytes, so placing them
                // at the top of 32 bits scales every depth the same way.
                let mut raw = [0u8; 4];
                if self.encoding == Encoding::BigEndianInt {
                    raw[..sample.len()].copy_from_slice(sample);
                } else {
                    for (slot, &byte) in raw.iter_mut().zip(sample.iter().rev()) {
                        *slot = byte;
                    }
                }
                (f64::from(i32::from_be_bytes(raw)) / 2_147_483_648.0) as f32
            }
        }
    }
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

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

/// What the chunks of an AIFF stream say about it.
pub(crate) struct AiffStream {
    pub sample_rate: u32,
    pub bits_per_sample: u16,
    pub is_float: bool,
    pub layout: PcmLayout,
    encoding: Encoding,
}

impl AiffStream {
    pub(crate) fn decoder(&self) -> AiffDecoder {
        AiffDecoder {
            encoding: self.encoding,
        }
    }
}

/// Reads the chunks of an AIFF stream, without reading its samples.
pub(crate) fn open<R: Read + Seek>(reader: &mut R) -> Result<AiffStream, AiffError> {
    let len = reader.seek(SeekFrom::End(0))?;
    if len < 12 {
        return Err(AiffError::NotAiff);
    }
    reader.seek(SeekFrom::Start(0))?;
    let mut form = [0u8; 12];
    reader.read_exact(&mut form)?;
    if &form[..4] != b"FORM" {
        return Err(AiffError::NotAiff);
    }
    let is_compressed_form = match &form[8..12] {
        b"AIFF" => false,
        b"AIFC" => true,
        _ => return Err(AiffError::NotAiff),
    };

    let mut common: Option<(usize, u64, u32, f64, Encoding)> = None;
    // Where the samples start, and how many bytes of them there are.
    let mut sound: Option<(u64, u64)> = None;
    let mut position = 12u64;
    while position.checked_add(8).is_some_and(|end| end <= len) {
        reader.seek(SeekFrom::Start(position))?;
        let mut header = [0u8; 8];
        reader.read_exact(&mut header)?;
        let id = &header[..4];
        let size = u64::from(u32_at(&header, 4));
        let start = position + 8;
        // A sound chunk that claims to run past the end is read as far as
        // the data goes: some writers leave the size unfinished.
        let end = start.saturating_add(size).min(len);
        let body_len = end - start;
        match id {
            b"COMM" => {
                // The fields of interest end with the compression type.
                let mut body = [0u8; 22];
                let have = body_len.min(22) as usize;
                reader.read_exact(&mut body[..have])?;
                let body = &body[..have];
                if body.len() < 18 {
                    return Err(AiffError::Malformed("common chunk too short"));
                }
                let channels = u16::from_be_bytes([body[0], body[1]]) as usize;
                let frames = u64::from(u32_at(body, 2));
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
                if body_len < 8 {
                    return Err(AiffError::Malformed("sound chunk too short"));
                }
                let mut offset = [0u8; 4];
                reader.read_exact(&mut offset)?;
                // The samples follow two numbers and `offset` unused bytes.
                let skip = 8 + u64::from(u32::from_be_bytes(offset));
                sound = Some(if skip <= body_len {
                    (start + skip, body_len - skip)
                } else {
                    (end, 0)
                });
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
    let (data_start, data_len) = sound.unwrap_or((len, 0));
    // Trust the data over the header if the two disagree.
    let frames = frames.min(data_len / (bytes_per_sample * channels) as u64);

    Ok(AiffStream {
        sample_rate: rate.round() as u32,
        bits_per_sample: match encoding {
            Encoding::Float32 => 32,
            Encoding::Float64 => 64,
            _ => bits as u16,
        },
        is_float: matches!(encoding, Encoding::Float32 | Encoding::Float64),
        layout: PcmLayout {
            channels,
            bytes_per_sample,
            data_start,
            frames,
        },
        encoding,
    })
}

fn parse(bytes: &[u8]) -> Result<AiffFile, AiffError> {
    let mut cursor = Cursor::new(bytes);
    let stream = open(&mut cursor)?;
    // The samples are in memory, so their count fits in a `usize`.
    let frames = usize::try_from(stream.layout.frames).map_err(|_| AiffError::TooLarge)?;
    let mut buffer = AudioBuffer::new(stream.layout.channels, frames);
    PcmReader::new(cursor, stream.decoder(), stream.layout).read(&mut buffer)?;

    Ok(AiffFile {
        buffer,
        sample_rate: stream.sample_rate,
        bits_per_sample: stream.bits_per_sample,
        is_float: stream.is_float,
    })
}

/// Length of the longest header [`header`] builds, the AIFF-C one.
pub(crate) const MAX_HEADER_LEN: usize = 60;

/// Whether `frames` frames taking `data_len` bytes fit the 32-bit sizes of
/// the format, with room for the header.
pub(crate) fn fits(frames: u64, data_len: u64) -> bool {
    frames <= u64::from(u32::MAX) && data_len < u64::from(u32::MAX) - 64
}

/// The number of frames a header states while the real number is unknown.
///
/// It is more than a file written here can hold: samples take at least two
/// bytes, and [`fits`] keeps the sample bytes under 4 GiB. A reader that
/// stops where the data stops therefore reads all there is. All ones would
/// do for this crate as well, but FFmpeg 7.1 refuses a file whose frame
/// count is all ones and reads one with this count to its end (checked by
/// hand; see `tests/data/README.md`).
const UNFINISHED_FRAMES: u32 = 0x7FFF_FFFF;

/// Builds everything that precedes the samples of an AIFF file, and returns
/// it with its length.
///
/// `sizes` is the number of frames and of sample bytes, or `None` while they
/// are unknown. The two chunk sizes are then all ones and the number of
/// frames is [`UNFINISHED_FRAMES`], all of which this crate reads as "up to
/// the end of the file", so that a recording cut short before its header
/// was completed can still be read.
pub(crate) fn header(
    channels: u16,
    sample_rate: u32,
    format: AiffFormat,
    sizes: Option<(u32, u32)>,
) -> ([u8; MAX_HEADER_LEN], usize) {
    let is_float = format.is_float();
    let bits = (format.bytes_per_sample() * 8) as u16;
    // AIFF-C adds a compression type and its name to the common chunk.
    let common_len: u32 = if is_float { 18 + 4 + 2 } else { 18 };
    let (frames, sound_len, form_len) = match sizes {
        Some((frames, data_len)) => {
            let padding = data_len % 2;
            let form_len = 4 + (8 + common_len) + (8 + 8 + data_len) + padding;
            (frames, 8 + data_len, form_len)
        }
        None => (UNFINISHED_FRAMES, u32::MAX, u32::MAX),
    };

    let mut header = [0u8; MAX_HEADER_LEN];
    header[0..4].copy_from_slice(b"FORM");
    header[4..8].copy_from_slice(&form_len.to_be_bytes());
    header[8..12].copy_from_slice(if is_float { b"AIFC" } else { b"AIFF" });

    header[12..16].copy_from_slice(b"COMM");
    header[16..20].copy_from_slice(&common_len.to_be_bytes());
    header[20..22].copy_from_slice(&channels.to_be_bytes());
    header[22..26].copy_from_slice(&frames.to_be_bytes());
    header[26..28].copy_from_slice(&bits.to_be_bytes());
    header[28..38].copy_from_slice(&write_extended(sample_rate));
    let mut position = 38;
    if is_float {
        // The type, and a name of no characters padded to an even length.
        header[38..42].copy_from_slice(b"fl32");
        position = 44;
    }

    header[position..position + 4].copy_from_slice(b"SSND");
    header[position + 4..position + 8].copy_from_slice(&sound_len.to_be_bytes());
    // An offset and a block size, both zero, fill the next eight bytes.
    (header, position + 16)
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
    let bytes_per_sample = format.bytes_per_sample();
    let frame_bytes = buffer.num_channels() * bytes_per_sample;
    let data_len = buffer
        .num_frames()
        .checked_mul(frame_bytes)
        .filter(|len| *len < u32::MAX as usize - 64)
        .ok_or(AiffError::TooLarge)?;

    let (header, header_len) = header(
        channels,
        sample_rate,
        format,
        Some((frames, data_len as u32)),
    );
    writer.write_all(&header[..header_len])?;

    let mut scratch = vec![0; pcm::scratch_len(frame_bytes, u64::from(frames))];
    pcm::write_planar(&mut writer, format, bytes_per_sample, buffer, &mut scratch)?;
    if data_len % 2 == 1 {
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

    #[test]
    fn an_unfinished_header_reads_to_the_end_of_the_file() {
        for format in [AiffFormat::Int16, AiffFormat::Float32] {
            let (header, header_len) = header(1, 8_000, format, None);
            assert_eq!(u32_at(&header, 4), u32::MAX);
            assert_eq!(u32_at(&header, 22), UNFINISHED_FRAMES);
            assert_eq!(u32_at(&header, header_len - 12), u32::MAX);
            // No file that was finished states this many frames: even one
            // channel of the narrowest samples would not fit.
            let frames = u64::from(UNFINISHED_FRAMES);
            assert_eq!(AiffFormat::Int16.bytes_per_sample(), 2);
            assert!(!fits(frames, frames * 2));
            assert!(!fits(frames - 31, (frames - 31) * 2));
            assert!(fits(frames - 32, (frames - 32) * 2));

            let width = format.bytes_per_sample();
            let mut bytes = header[..header_len].to_vec();
            let mut sample = [0u8; 4];
            for value in [0.5f32, -0.5] {
                format.encode(value, &mut sample[..width]);
                bytes.extend_from_slice(&sample[..width]);
            }
            // Part of a third sample, as if the writer had been killed.
            bytes.push(0x12);
            let file = read(bytes.as_slice()).unwrap();
            assert_eq!(file.buffer.channel(0), &[0.5, -0.5], "{format:?}");
        }
    }
}
