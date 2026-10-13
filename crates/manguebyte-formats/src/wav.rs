//! RIFF/WAVE files.
//!
//! Reads 8, 16, 24 and 32-bit integer PCM and 32 and 64-bit float, including
//! the `WAVE_FORMAT_EXTENSIBLE` variants. Writes 16 and 24-bit PCM and 32-bit
//! float.
//!
//! ```
//! use manguebyte_core::AudioBuffer;
//! use manguebyte_formats::wav::{self, WavFormat};
//!
//! let mut buffer = AudioBuffer::<f32>::new(1, 4);
//! buffer.channel_mut(0).copy_from_slice(&[0.0, 0.5, -0.5, 1.0]);
//!
//! let mut bytes = Vec::new();
//! wav::write(&mut bytes, &buffer, 48_000, WavFormat::Float32).unwrap();
//!
//! let file = wav::read(bytes.as_slice()).unwrap();
//! assert_eq!(file.sample_rate, 48_000);
//! assert_eq!(file.buffer, buffer);
//! ```

use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

use manguebyte_core::AudioBuffer;

const FORMAT_PCM: u16 = 1;
const FORMAT_FLOAT: u16 = 3;
const FORMAT_EXTENSIBLE: u16 = 0xFFFE;

/// How samples are stored in a WAV file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WavFormat {
    /// 16-bit signed integer PCM. The usual choice for distribution.
    Int16,
    /// 24-bit signed integer PCM.
    Int24,
    /// 32-bit IEEE float. Lossless for `f32` buffers and keeps values beyond
    /// full scale.
    Float32,
}

impl WavFormat {
    fn bytes_per_sample(self) -> usize {
        match self {
            Self::Int16 => 2,
            Self::Int24 => 3,
            Self::Float32 => 4,
        }
    }
}

/// The contents of a WAV file.
#[derive(Debug, Clone, PartialEq)]
pub struct WavFile {
    /// The audio, converted to `f32` with integer full scale mapped to `±1.0`.
    pub buffer: AudioBuffer<f32>,
    /// Sample rate in hertz.
    pub sample_rate: u32,
    /// Bit depth the samples were stored with.
    pub bits_per_sample: u16,
    /// Whether the samples were stored as floating point.
    pub is_float: bool,
}

/// What went wrong while reading or writing a WAV file.
#[derive(Debug)]
pub enum WavError {
    /// The underlying reader or writer failed.
    Io(io::Error),
    /// The data is not a RIFF/WAVE file.
    NotWav,
    /// The file is damaged or incomplete.
    Malformed(&'static str),
    /// The file is valid but uses an encoding this crate does not handle.
    Unsupported(&'static str),
    /// The audio is too long for the 4 GiB limit of the WAV format.
    TooLarge,
}

impl fmt::Display for WavError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::NotWav => f.write_str("not a RIFF/WAVE file"),
            Self::Malformed(what) => write!(f, "malformed WAV file: {what}"),
            Self::Unsupported(what) => write!(f, "unsupported WAV file: {what}"),
            Self::TooLarge => f.write_str("audio exceeds the 4 GiB WAV size limit"),
        }
    }
}

impl std::error::Error for WavError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for WavError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Reads a WAV file from `reader`. The whole stream is loaded into memory.
pub fn read<R: Read>(mut reader: R) -> Result<WavFile, WavError> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    parse(&bytes)
}

/// Reads the WAV file at `path`.
pub fn read_file<P: AsRef<Path>>(path: P) -> Result<WavFile, WavError> {
    read(BufReader::new(File::open(path)?))
}

/// Writes `buffer` to `writer` as a WAV file.
///
/// Integer formats clip samples outside `-1.0..=1.0`.
pub fn write<W: Write>(
    mut writer: W,
    buffer: &AudioBuffer<f32>,
    sample_rate: u32,
    format: WavFormat,
) -> Result<(), WavError> {
    let channels = u16::try_from(buffer.num_channels()).map_err(|_| WavError::TooLarge)?;
    if channels == 0 {
        return Err(WavError::Unsupported(
            "a WAV file needs at least one channel",
        ));
    }
    let bytes_per_sample = format.bytes_per_sample();
    let block_align = usize::from(channels) * bytes_per_sample;
    let data_len = buffer
        .num_frames()
        .checked_mul(block_align)
        .and_then(|len| u32::try_from(len).ok())
        // Leave room for the header and a padding byte in the RIFF size field.
        .filter(|len| *len < u32::MAX - 36)
        .ok_or(WavError::TooLarge)?;
    let padding = data_len % 2;

    let format_tag = match format {
        WavFormat::Int16 | WavFormat::Int24 => FORMAT_PCM,
        WavFormat::Float32 => FORMAT_FLOAT,
    };

    writer.write_all(b"RIFF")?;
    writer.write_all(&(36 + data_len + padding).to_le_bytes())?;
    writer.write_all(b"WAVE")?;

    writer.write_all(b"fmt ")?;
    writer.write_all(&16u32.to_le_bytes())?;
    writer.write_all(&format_tag.to_le_bytes())?;
    writer.write_all(&channels.to_le_bytes())?;
    writer.write_all(&sample_rate.to_le_bytes())?;
    writer.write_all(&sample_rate.saturating_mul(block_align as u32).to_le_bytes())?;
    writer.write_all(&(block_align as u16).to_le_bytes())?;
    writer.write_all(&((bytes_per_sample * 8) as u16).to_le_bytes())?;

    writer.write_all(b"data")?;
    writer.write_all(&data_len.to_le_bytes())?;
    for frame in 0..buffer.num_frames() {
        for channel in buffer.iter_channels() {
            let sample = channel[frame];
            match format {
                WavFormat::Int16 => {
                    let value = (sample.clamp(-1.0, 1.0) * 32_767.0).round() as i16;
                    writer.write_all(&value.to_le_bytes())?;
                }
                WavFormat::Int24 => {
                    let value = (f64::from(sample.clamp(-1.0, 1.0)) * 8_388_607.0).round() as i32;
                    writer.write_all(&value.to_le_bytes()[..3])?;
                }
                WavFormat::Float32 => writer.write_all(&sample.to_le_bytes())?,
            }
        }
    }
    if padding == 1 {
        writer.write_all(&[0])?;
    }
    writer.flush()?;
    Ok(())
}

/// Writes `buffer` to a new WAV file at `path`, replacing any existing file.
pub fn write_file<P: AsRef<Path>>(
    path: P,
    buffer: &AudioBuffer<f32>,
    sample_rate: u32,
    format: WavFormat,
) -> Result<(), WavError> {
    write(
        BufWriter::new(File::create(path)?),
        buffer,
        sample_rate,
        format,
    )
}

struct Layout {
    format_tag: u16,
    channels: usize,
    sample_rate: u32,
    block_align: usize,
    bits_per_sample: u16,
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn parse_fmt(chunk: &[u8]) -> Result<Layout, WavError> {
    if chunk.len() < 16 {
        return Err(WavError::Malformed("fmt chunk is too short"));
    }
    let mut format_tag = u16_at(chunk, 0);
    if format_tag == FORMAT_EXTENSIBLE {
        if chunk.len() < 40 {
            return Err(WavError::Malformed("extensible fmt chunk is too short"));
        }
        // The real format is the first field of the sub-format GUID.
        format_tag = u16_at(chunk, 24);
    }
    Ok(Layout {
        format_tag,
        channels: usize::from(u16_at(chunk, 2)),
        sample_rate: u32_at(chunk, 4),
        block_align: usize::from(u16_at(chunk, 12)),
        bits_per_sample: u16_at(chunk, 14),
    })
}

fn parse(bytes: &[u8]) -> Result<WavFile, WavError> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(WavError::NotWav);
    }

    let mut layout = None;
    let mut data = None;
    let mut position = 12;
    while position + 8 <= bytes.len() {
        let id = &bytes[position..position + 4];
        let size = u32_at(bytes, position + 4) as usize;
        let start = position + 8;
        // Tolerate a final chunk whose declared size overruns the file, as
        // produced by recorders that were interrupted.
        let end = start.saturating_add(size).min(bytes.len());
        match id {
            b"fmt " => layout = Some(parse_fmt(&bytes[start..end])?),
            b"data" => data = Some(&bytes[start..end]),
            _ => {}
        }
        if layout.is_some() && data.is_some() {
            break;
        }
        // Chunks are padded to an even length.
        position = start.saturating_add(size).saturating_add(size % 2);
    }

    let layout = layout.ok_or(WavError::Malformed("missing fmt chunk"))?;
    let data = data.ok_or(WavError::Malformed("missing data chunk"))?;

    let is_float = match layout.format_tag {
        FORMAT_PCM => false,
        FORMAT_FLOAT => true,
        _ => {
            return Err(WavError::Unsupported(
                "only PCM and IEEE float are supported",
            ));
        }
    };
    match (is_float, layout.bits_per_sample) {
        (false, 8 | 16 | 24 | 32) | (true, 32 | 64) => {}
        _ => return Err(WavError::Unsupported("unsupported bit depth")),
    }
    if layout.channels == 0 {
        return Err(WavError::Malformed("file declares zero channels"));
    }
    let bytes_per_sample = usize::from(layout.bits_per_sample / 8);
    if layout.block_align != layout.channels * bytes_per_sample {
        return Err(WavError::Malformed(
            "block alignment does not match the format",
        ));
    }

    let frames = data.len() / layout.block_align;
    let mut interleaved = Vec::with_capacity(frames * layout.channels);
    for sample in data[..frames * layout.block_align].chunks_exact(bytes_per_sample) {
        interleaved.push(decode_sample(sample, is_float));
    }

    Ok(WavFile {
        buffer: AudioBuffer::from_interleaved(&interleaved, layout.channels),
        sample_rate: layout.sample_rate,
        bits_per_sample: layout.bits_per_sample,
        is_float,
    })
}

/// Decodes one little-endian sample; the slice length selects the bit depth.
fn decode_sample(bytes: &[u8], is_float: bool) -> f32 {
    match (is_float, bytes) {
        (false, &[a]) => (f32::from(a) - 128.0) / 128.0,
        (false, &[a, b]) => f32::from(i16::from_le_bytes([a, b])) / 32_768.0,
        (false, &[a, b, c]) => {
            // Shifting up and back down sign-extends the 24-bit value.
            let value = i32::from_le_bytes([0, a, b, c]) >> 8;
            value as f32 / 8_388_608.0
        }
        (false, &[a, b, c, d]) => {
            (f64::from(i32::from_le_bytes([a, b, c, d])) / 2_147_483_648.0) as f32
        }
        (true, &[a, b, c, d]) => f32::from_le_bytes([a, b, c, d]),
        (true, &[a, b, c, d, e, f, g, h]) => f64::from_le_bytes([a, b, c, d, e, f, g, h]) as f32,
        _ => unreachable!("bit depth was validated before decoding"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo_ramp() -> AudioBuffer<f32> {
        let mut buffer = AudioBuffer::new(2, 5);
        buffer
            .channel_mut(0)
            .copy_from_slice(&[0.0, 0.25, 0.5, -0.5, -1.0]);
        buffer
            .channel_mut(1)
            .copy_from_slice(&[1.0, -0.25, 0.125, 0.0, 0.75]);
        buffer
    }

    fn round_trip(buffer: &AudioBuffer<f32>, format: WavFormat) -> WavFile {
        let mut bytes = Vec::new();
        write(&mut bytes, buffer, 44_100, format).unwrap();
        read(bytes.as_slice()).unwrap()
    }

    fn assert_close(a: &AudioBuffer<f32>, b: &AudioBuffer<f32>, tolerance: f32) {
        assert_eq!(a.num_channels(), b.num_channels());
        assert_eq!(a.num_frames(), b.num_frames());
        for (left, right) in a.iter_channels().zip(b.iter_channels()) {
            for (x, y) in left.iter().zip(right) {
                assert!((x - y).abs() <= tolerance, "{x} vs {y}");
            }
        }
    }

    #[test]
    fn float_round_trip_is_exact() {
        let buffer = stereo_ramp();
        let file = round_trip(&buffer, WavFormat::Float32);
        assert_eq!(file.buffer, buffer);
        assert_eq!(file.sample_rate, 44_100);
        assert_eq!(file.bits_per_sample, 32);
        assert!(file.is_float);
    }

    #[test]
    fn integer_round_trips_stay_within_one_step() {
        let buffer = stereo_ramp();

        let file = round_trip(&buffer, WavFormat::Int16);
        assert_eq!(file.bits_per_sample, 16);
        assert!(!file.is_float);
        assert_close(&file.buffer, &buffer, 1.0 / 32_768.0);

        let file = round_trip(&buffer, WavFormat::Int24);
        assert_eq!(file.bits_per_sample, 24);
        assert_close(&file.buffer, &buffer, 1.0 / 8_388_608.0);
    }

    #[test]
    fn integer_formats_clip() {
        let mut buffer = AudioBuffer::new(1, 2);
        buffer.channel_mut(0).copy_from_slice(&[4.0, -4.0]);
        let file = round_trip(&buffer, WavFormat::Int16);
        assert!((file.buffer.channel(0)[0] - 1.0).abs() < 1e-4);
        assert!((file.buffer.channel(0)[1] + 1.0).abs() < 1e-4);
    }

    #[test]
    fn header_is_canonical() {
        let mut bytes = Vec::new();
        write(&mut bytes, &stereo_ramp(), 48_000, WavFormat::Int16).unwrap();
        assert_eq!(bytes.len(), 44 + 5 * 2 * 2);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(u32_at(&bytes, 4) as usize, bytes.len() - 8);
        assert_eq!(&bytes[8..16], b"WAVEfmt ");
        assert_eq!(u16_at(&bytes, 20), FORMAT_PCM);
        assert_eq!(u16_at(&bytes, 22), 2);
        assert_eq!(u32_at(&bytes, 24), 48_000);
        assert_eq!(u32_at(&bytes, 28), 48_000 * 4);
        assert_eq!(u16_at(&bytes, 32), 4);
        assert_eq!(u16_at(&bytes, 34), 16);
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(u32_at(&bytes, 40), 20);
    }

    #[test]
    fn odd_sized_data_is_padded() {
        let buffer = AudioBuffer::<f32>::new(1, 3);
        let mut bytes = Vec::new();
        write(&mut bytes, &buffer, 8_000, WavFormat::Int24).unwrap();
        assert_eq!(bytes.len(), 44 + 9 + 1);
        assert_eq!(u32_at(&bytes, 40), 9);
        assert_eq!(read(bytes.as_slice()).unwrap().buffer.num_frames(), 3);
    }

    #[test]
    fn skips_unknown_chunks_and_reads_eight_bit() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        // An odd-sized chunk, to exercise the padding rule.
        bytes.extend_from_slice(b"LIST");
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&[1, 2, 3, 0]);
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&FORMAT_PCM.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&8_000u32.to_le_bytes());
        bytes.extend_from_slice(&8_000u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&8u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&[128, 255, 0]);

        let file = read(bytes.as_slice()).unwrap();
        assert_eq!(file.sample_rate, 8_000);
        assert_eq!(file.buffer.channel(0), &[0.0, 127.0 / 128.0, -1.0]);
    }

    #[test]
    fn reports_bad_input() {
        assert!(matches!(read(&b"not a wav"[..]), Err(WavError::NotWav)));
        assert!(matches!(
            read(&b"RIFF\0\0\0\0WAVE"[..]),
            Err(WavError::Malformed(_))
        ));

        let empty = AudioBuffer::<f32>::new(0, 0);
        assert!(matches!(
            write(Vec::new(), &empty, 44_100, WavFormat::Int16),
            Err(WavError::Unsupported(_))
        ));
    }
}
