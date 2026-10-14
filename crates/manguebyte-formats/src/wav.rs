//! RIFF/WAVE files.
//!
//! Reads 8, 16, 24 and 32-bit integer PCM and 32 and 64-bit float, including
//! the `WAVE_FORMAT_EXTENSIBLE` variants and RF64 files, which carry 64-bit
//! sizes for audio beyond 4 GiB. Writes 16 and 24-bit PCM and 32-bit float.
//!
//! The functions here hold a whole file in memory. To read or write block by
//! block, use [`AudioReader`](crate::AudioReader) and
//! [`AudioWriter`](crate::AudioWriter).
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
use std::io::{self, BufReader, BufWriter, Cursor, Read, Seek, SeekFrom, Write};
use std::path::Path;

use manguebyte_core::AudioBuffer;

use crate::pcm::{self, PcmLayout, PcmReader, SampleDecoder, SampleEncoder};

const FORMAT_PCM: u16 = 1;
const FORMAT_FLOAT: u16 = 3;
const FORMAT_EXTENSIBLE: u16 = 0xFFFE;

/// What a 32-bit size field holds when the real size is in the `ds64` chunk
/// of an RF64 file.
const SIZE_IN_DS64: u32 = 0xFFFF_FFFF;

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
    pub(crate) fn bytes_per_sample(self) -> usize {
        match self {
            Self::Int16 => 2,
            Self::Int24 => 3,
            Self::Float32 => 4,
        }
    }
}

impl SampleEncoder for WavFormat {
    fn encode(self, sample: f32, bytes: &mut [u8]) {
        match self {
            Self::Int16 => {
                let value = (sample.clamp(-1.0, 1.0) * 32_767.0).round() as i16;
                bytes.copy_from_slice(&value.to_le_bytes());
            }
            Self::Int24 => {
                let value = (f64::from(sample.clamp(-1.0, 1.0)) * 8_388_607.0).round() as i32;
                bytes.copy_from_slice(&value.to_le_bytes()[..3]);
            }
            Self::Float32 => bytes.copy_from_slice(&sample.to_le_bytes()),
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

    writer.write_all(b"RIFF")?;
    writer.write_all(&(36 + data_len + padding).to_le_bytes())?;
    writer.write_all(b"WAVE")?;
    writer.write_all(&fmt_chunk(channels, sample_rate, format))?;
    writer.write_all(b"data")?;
    writer.write_all(&data_len.to_le_bytes())?;

    let mut scratch = vec![0; pcm::scratch_len(block_align, buffer.num_frames() as u64)];
    pcm::write_planar(&mut writer, format, bytes_per_sample, buffer, &mut scratch)?;
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

/// The `fmt ` chunk, with its own header, for audio in `format`.
fn fmt_chunk(channels: u16, sample_rate: u32, format: WavFormat) -> [u8; 24] {
    let bytes_per_sample = format.bytes_per_sample();
    let block_align = usize::from(channels) * bytes_per_sample;
    let format_tag = match format {
        WavFormat::Int16 | WavFormat::Int24 => FORMAT_PCM,
        WavFormat::Float32 => FORMAT_FLOAT,
    };
    let byte_rate = sample_rate.saturating_mul(block_align as u32);

    let mut chunk = [0u8; 24];
    chunk[0..4].copy_from_slice(b"fmt ");
    chunk[4..8].copy_from_slice(&16u32.to_le_bytes());
    chunk[8..10].copy_from_slice(&format_tag.to_le_bytes());
    chunk[10..12].copy_from_slice(&channels.to_le_bytes());
    chunk[12..16].copy_from_slice(&sample_rate.to_le_bytes());
    chunk[16..20].copy_from_slice(&byte_rate.to_le_bytes());
    chunk[20..22].copy_from_slice(&(block_align as u16).to_le_bytes());
    chunk[22..24].copy_from_slice(&((bytes_per_sample * 8) as u16).to_le_bytes());
    chunk
}

/// Length of the header [`stream_header`] builds.
pub(crate) const STREAM_HEADER_LEN: usize = 80;

/// The RIFF size of a file that starts with a [`stream_header`]: the form
/// type, three chunks and the padding byte that follows an odd number of
/// sample bytes.
fn stream_riff_size(data_len: u64) -> u64 {
    (STREAM_HEADER_LEN as u64 - 8)
        .saturating_add(data_len)
        .saturating_add(data_len % 2)
}

/// Whether `data_len` bytes of samples behind a [`stream_header`] need the
/// 64-bit sizes of RF64.
///
/// A size of all ones is never written as a real size: in RF64 it means
/// "look in the `ds64` chunk".
pub(crate) fn needs_rf64(data_len: u64) -> bool {
    stream_riff_size(data_len) >= u64::from(SIZE_IN_DS64)
}

/// Builds the header of a WAV file whose length is not known when writing
/// starts.
///
/// Between the form type and the format sits a chunk of 28 bytes that
/// readers skip (`JUNK`). It reserves the room of the `ds64` chunk of RF64,
/// as EBU Tech 3306 recommends, so that a file which outgrows the 32-bit
/// sizes of RIFF becomes RF64 by rewriting this header alone.
///
/// `data_len` is the number of sample bytes, or `None` while it is unknown.
/// The sizes are then all ones, which this crate and other tolerant readers
/// take to mean "up to the end of the file", so that a recording cut short
/// before its header was completed can still be read.
pub(crate) fn stream_header(
    channels: u16,
    sample_rate: u32,
    format: WavFormat,
    data_len: Option<u64>,
    always_rf64: bool,
) -> [u8; STREAM_HEADER_LEN] {
    let mut header = [0u8; STREAM_HEADER_LEN];
    header[8..12].copy_from_slice(b"WAVE");
    header[16..20].copy_from_slice(&28u32.to_le_bytes());
    header[48..72].copy_from_slice(&fmt_chunk(channels, sample_rate, format));
    header[72..76].copy_from_slice(b"data");

    match data_len {
        Some(data_len) if always_rf64 || needs_rf64(data_len) => {
            let block_align = u64::from(channels) * format.bytes_per_sample() as u64;
            header[0..4].copy_from_slice(b"RF64");
            header[4..8].copy_from_slice(&SIZE_IN_DS64.to_le_bytes());
            header[12..16].copy_from_slice(b"ds64");
            header[20..28].copy_from_slice(&stream_riff_size(data_len).to_le_bytes());
            header[28..36].copy_from_slice(&data_len.to_le_bytes());
            // The count of sample frames, as a `fact` chunk would give it.
            header[36..44].copy_from_slice(&(data_len / block_align.max(1)).to_le_bytes());
            // No table of further chunk sizes: bytes 44..48 stay zero.
            header[76..80].copy_from_slice(&SIZE_IN_DS64.to_le_bytes());
        }
        _ => {
            let (riff_size, data_size) = match data_len {
                Some(data_len) => (stream_riff_size(data_len) as u32, data_len as u32),
                None => (SIZE_IN_DS64, SIZE_IN_DS64),
            };
            header[0..4].copy_from_slice(b"RIFF");
            header[4..8].copy_from_slice(&riff_size.to_le_bytes());
            header[12..16].copy_from_slice(b"JUNK");
            header[76..80].copy_from_slice(&data_size.to_le_bytes());
        }
    }
    header
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

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from(u32_at(bytes, offset)) | (u64::from(u32_at(bytes, offset + 4)) << 32)
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

/// What the header of a WAV stream says about it.
pub(crate) struct WavStream {
    pub sample_rate: u32,
    pub bits_per_sample: u16,
    pub is_float: bool,
    pub layout: PcmLayout,
}

impl WavStream {
    pub(crate) fn decoder(&self) -> WavDecoder {
        WavDecoder {
            is_float: self.is_float,
        }
    }
}

/// Reads the chunks of a WAV stream up to its samples, without reading the
/// samples themselves.
pub(crate) fn open<R: Read + Seek>(reader: &mut R) -> Result<WavStream, WavError> {
    let len = reader.seek(SeekFrom::End(0))?;
    if len < 12 {
        return Err(WavError::NotWav);
    }
    reader.seek(SeekFrom::Start(0))?;
    let mut form = [0u8; 12];
    reader.read_exact(&mut form)?;
    let is_rf64 = match &form[0..4] {
        b"RIFF" => false,
        b"RF64" => true,
        _ => return Err(WavError::NotWav),
    };
    if &form[8..12] != b"WAVE" {
        return Err(WavError::NotWav);
    }

    let mut layout = None;
    let mut data = None;
    // The size of the samples of an RF64 file, where 32 bits are too few.
    let mut long_data_len = None;
    let mut position = 12u64;
    while position.checked_add(8).is_some_and(|end| end <= len) {
        reader.seek(SeekFrom::Start(position))?;
        let mut header = [0u8; 8];
        reader.read_exact(&mut header)?;
        let id = &header[0..4];
        let start = position + 8;
        let mut size = u64::from(u32_at(&header, 4));
        if id == b"data" && size == u64::from(SIZE_IN_DS64) {
            // All ones is not a size. In an RF64 file it says that the size
            // is in the `ds64` chunk. Where there is none, the header was
            // never completed and the samples run to the end of the file,
            // which may be further than 32 bits can say.
            size = long_data_len.unwrap_or(len - start);
        }
        // Tolerate a final chunk whose declared size overruns the file, as
        // produced by recorders that were interrupted.
        let end = start.saturating_add(size).min(len);
        match id {
            b"fmt " => {
                // Nothing past the fortieth byte is of interest.
                let mut body = [0u8; 40];
                let have = (end - start).min(40) as usize;
                reader.read_exact(&mut body[..have])?;
                layout = Some(parse_fmt(&body[..have])?);
            }
            b"data" => data = Some((start, end - start)),
            b"ds64" if is_rf64 => {
                // The RIFF size, the data size and the count of frames.
                if end - start < 24 {
                    return Err(WavError::Malformed("ds64 chunk is too short"));
                }
                let mut body = [0u8; 24];
                reader.read_exact(&mut body)?;
                long_data_len = Some(u64_at(&body, 8));
            }
            _ => {}
        }
        if layout.is_some() && data.is_some() {
            break;
        }
        // Chunks are padded to an even length.
        position = start.saturating_add(size).saturating_add(size % 2);
    }

    let layout = layout.ok_or(WavError::Malformed("missing fmt chunk"))?;
    let (data_start, data_len) = data.ok_or(WavError::Malformed("missing data chunk"))?;

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

    Ok(WavStream {
        sample_rate: layout.sample_rate,
        bits_per_sample: layout.bits_per_sample,
        is_float,
        layout: PcmLayout {
            channels: layout.channels,
            bytes_per_sample,
            data_start,
            frames: data_len / layout.block_align as u64,
        },
    })
}

fn parse(bytes: &[u8]) -> Result<WavFile, WavError> {
    let mut cursor = Cursor::new(bytes);
    let stream = open(&mut cursor)?;
    // The samples are in memory, so their count fits in a `usize`.
    let frames = usize::try_from(stream.layout.frames).map_err(|_| WavError::TooLarge)?;
    let mut buffer = AudioBuffer::new(stream.layout.channels, frames);
    PcmReader::new(cursor, stream.decoder(), stream.layout).read(&mut buffer)?;

    Ok(WavFile {
        buffer,
        sample_rate: stream.sample_rate,
        bits_per_sample: stream.bits_per_sample,
        is_float: stream.is_float,
    })
}

/// Decodes the samples of a WAV stream.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WavDecoder {
    is_float: bool,
}

impl SampleDecoder for WavDecoder {
    fn decode(self, bytes: &[u8]) -> f32 {
        decode_sample(bytes, self.is_float)
    }
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

    /// The fields of a streamed header, read back the way a reader would.
    struct Sizes {
        form: [u8; 4],
        riff_size: u32,
        reserved: [u8; 4],
        long_riff_size: u64,
        long_data_size: u64,
        long_frames: u64,
        data_size: u32,
    }

    fn sizes(header: &[u8; STREAM_HEADER_LEN]) -> Sizes {
        assert_eq!(&header[8..12], b"WAVE");
        assert_eq!(u32_at(header, 16), 28, "the reserved chunk holds 28 bytes");
        assert_eq!(u32_at(header, 44), 0, "no table of chunk sizes");
        assert_eq!(&header[48..52], b"fmt ");
        assert_eq!(&header[72..76], b"data");
        Sizes {
            form: [header[0], header[1], header[2], header[3]],
            riff_size: u32_at(header, 4),
            reserved: [header[12], header[13], header[14], header[15]],
            long_riff_size: u64_at(header, 20),
            long_data_size: u64_at(header, 28),
            long_frames: u64_at(header, 36),
            data_size: u32_at(header, 76),
        }
    }

    #[test]
    fn a_streamed_header_stays_riff_while_32_bits_are_enough() {
        for data_len in [0u64, 4, 4_000, 0xFFFF_FFB4, 0xFFFF_FFB6] {
            let header = stream_header(2, 48_000, WavFormat::Int16, Some(data_len), false);
            let sizes = sizes(&header);
            assert_eq!(&sizes.form, b"RIFF", "{data_len}");
            assert_eq!(&sizes.reserved, b"JUNK", "{data_len}");
            assert_eq!(u64::from(sizes.riff_size), 72 + data_len, "{data_len}");
            assert_eq!(u64::from(sizes.data_size), data_len, "{data_len}");
            // The reserved room is left empty.
            assert!(header[20..48].iter().all(|&byte| byte == 0));
        }
        // The largest size a RIFF header can state: one below all ones.
        assert!(!needs_rf64(0xFFFF_FFB6));
        assert!(needs_rf64(0xFFFF_FFB7));
    }

    #[test]
    fn a_streamed_header_becomes_rf64_past_four_gigabytes() {
        // The first length that no longer fits, 4 GiB, and an hour of
        // 32-channel audio at 192 kHz in 24 bits: 66 GB.
        for data_len in [0xFFFF_FFB8u64, 1 << 32, 192_000 * 3_600 * 32 * 3] {
            for format in [WavFormat::Int16, WavFormat::Int24, WavFormat::Float32] {
                let channels = 32u16;
                let block_align = u64::from(channels) * format.bytes_per_sample() as u64;
                let data_len = data_len.next_multiple_of(block_align);
                let header = stream_header(channels, 192_000, format, Some(data_len), false);
                let sizes = sizes(&header);
                assert_eq!(&sizes.form, b"RF64");
                assert_eq!(&sizes.reserved, b"ds64");
                assert_eq!(sizes.riff_size, 0xFFFF_FFFF);
                assert_eq!(sizes.data_size, 0xFFFF_FFFF);
                assert_eq!(sizes.long_data_size, data_len);
                assert_eq!(sizes.long_frames, data_len / block_align);
                // Everything after the first eight bytes of the file.
                let file_len = STREAM_HEADER_LEN as u64 + data_len + data_len % 2;
                assert_eq!(sizes.long_riff_size, file_len - 8);
            }
        }
    }

    #[test]
    fn a_streamed_header_can_be_rf64_at_any_length_and_reads_back() {
        let header = stream_header(1, 8_000, WavFormat::Int24, Some(9), true);
        let sizes = sizes(&header);
        assert_eq!(&sizes.form, b"RF64");
        assert_eq!(sizes.long_data_size, 9);
        assert_eq!(sizes.long_frames, 3);
        // Nine bytes of samples are followed by one of padding.
        assert_eq!(sizes.long_riff_size, 72 + 9 + 1);

        let mut bytes = header.to_vec();
        bytes.extend_from_slice(&[0, 0, 0x40, 0, 0, 0xC0, 0xFF, 0xFF, 0x7F, 0]);
        let file = read(bytes.as_slice()).unwrap();
        assert_eq!(file.bits_per_sample, 24);
        assert_eq!(
            file.buffer.channel(0),
            &[0.5, -0.5, 8_388_607.0 / 8_388_608.0]
        );
    }

    #[test]
    fn an_unfinished_streamed_header_reads_to_the_end_of_the_file() {
        let header = stream_header(1, 8_000, WavFormat::Int16, None, false);
        let sizes = sizes(&header);
        assert_eq!(&sizes.form, b"RIFF");
        assert_eq!(sizes.riff_size, 0xFFFF_FFFF);
        assert_eq!(sizes.data_size, 0xFFFF_FFFF);

        let mut bytes = header.to_vec();
        // Two samples and half of a third, as if the writer had been killed.
        bytes.extend_from_slice(&[0x00, 0x40, 0x00, 0xC0, 0x12]);
        let file = read(bytes.as_slice()).unwrap();
        assert_eq!(file.buffer.channel(0), &[0.5, -0.5]);
    }
}
